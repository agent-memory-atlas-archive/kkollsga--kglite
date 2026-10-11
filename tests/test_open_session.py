"""``kglite.open_session(path, durable=...)``: a durable, thread-safe ``Session``.

The session opens through the same core entry (``open_path``) the servers use:
it owns the writer lease, replays the log, and writes through the core's
auto-commit with the GIL released. These tests pin the recovery contract
(crash, then reopen through either entry point), the lifecycle (lease, close,
fences) and the concurrency contract (exact counts, no partial reads).

Not asserted: that threaded ``full`` writers share fsyncs. The barrier counter
is a ``test-seam``-only core hook that the wheel does not build, so the grouping
itself is covered by the core's ``group_commit_tests``; here the threaded cases
pin exact counts and the absence of errors.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import textwrap
import threading
import time

import pytest

import kglite
from tests.fixtures import build_durable_golden_fixtures as golden_v10
from tests.fixtures import build_durable_golden_v11_fixtures as golden_v11

REPO_ROOT = Path(__file__).resolve().parents[1]
IDS = "MATCH (n:N) RETURN n.id AS id ORDER BY id"


def _child(body: str, path: Path, **extra) -> subprocess.CompletedProcess:
    """Run ``body`` in a fresh interpreter importing this tree, with ``path`` bound."""
    script = "import kglite, os, sys\npath = sys.argv[1]\n" + textwrap.dedent(body)
    return subprocess.run(
        [sys.executable, "-c", script, str(path)],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=60,
        **extra,
    )


def _ids(graph) -> list[int]:
    return [row["id"] for row in graph.cypher(IDS).to_list()]


def _crashed_graph(tmp_path: Path, level: str, storage: str | None = None) -> Path:
    """A path whose session wrote 25 nodes and then died without closing."""
    path = tmp_path / "app.kgl"
    kwargs = f", storage={storage!r}" if storage else ""
    result = _child(
        f"""
        s = kglite.open_session(path, durable={level!r}{kwargs})
        for i in range(25):
            s.execute("CREATE (:N {{id: $i}})", params={{"i": i}})
        os._exit(0)
        """,
        path,
    )
    assert result.returncode == 0, result.stderr
    assert (tmp_path / "app.kgl-wal").exists(), "the child must leave a log behind"
    return path


def _copy_dir(src: Path, dst: Path) -> Path:
    shutil.copytree(src.parent, dst, ignore=shutil.ignore_patterns("*.lock-owner"))
    return dst / src.name


# --- crash recovery ---------------------------------------------------------


@pytest.mark.parametrize("level", ["full", "normal"])
def test_a_crash_replays_session_writes_through_either_entry_point(tmp_path, level):
    path = _crashed_graph(tmp_path, level)
    via_open = _copy_dir(path, tmp_path / "via_open")

    s = kglite.open_session(str(path), durable=level)
    assert _ids(s) == list(range(25))
    s.close()

    g = kglite.open(str(via_open), durable=level)
    assert _ids(g) == list(range(25))
    g.close()


def test_a_kglite_open_crash_replays_through_open_session(tmp_path):
    path = tmp_path / "app.kgl"
    result = _child(
        """
        g = kglite.open(path, durable="full")
        for i in range(10):
            g.cypher("CREATE (:N {id: $i})", params={"i": i})
        os._exit(0)
        """,
        path,
    )
    assert result.returncode == 0, result.stderr
    with kglite.open_session(str(path), durable="full") as s:
        assert _ids(s) == list(range(10))


def test_the_log_is_what_carries_the_writes(tmp_path):
    """Non-vacuity: without the sidecar the crashed session's writes are gone."""
    path = _crashed_graph(tmp_path, "full")
    (tmp_path / "app.kgl-wal").unlink()
    with kglite.open_session(str(path), durable="full") as s:
        assert _ids(s) == []


@pytest.mark.parametrize("name", sorted(golden_v10.SCENARIOS))
def test_golden_v10_logs_replay_through_open_session(name, tmp_path):
    target = tmp_path / name
    shutil.copytree(golden_v10.FIXTURE_DIR / name, target)
    s = kglite.open_session(str(target / "app.kgl"), durable=golden_v10.SCENARIOS[name]["level"])
    try:
        assert golden_v10.compare(name, golden_v10.observe(name, s.cursor())) == []
    finally:
        s.close()


@pytest.mark.parametrize("name", sorted(golden_v11.SCENARIOS))
def test_golden_v11_logs_replay_through_open_session(name, tmp_path):
    target = tmp_path / name
    shutil.copytree(golden_v11.FIXTURE_DIR / name, target)
    s = kglite.open_session(str(target / "app.kgl"), durable=golden_v11.SCENARIOS[name]["level"])
    try:
        assert golden_v11.compare(name, golden_v11.observe(name, s.cursor())) == []
    finally:
        s.close()


def test_mapped_storage_replays_after_a_crash(tmp_path):
    path = _crashed_graph(tmp_path, "full", storage="mapped")
    with kglite.open_session(str(path), durable="full", storage="mapped") as s:
        assert _ids(s) == list(range(25))


# --- concurrency ------------------------------------------------------------


@pytest.mark.parametrize("level", ["full", "normal"])
def test_threaded_writers_lose_nothing(tmp_path, level):
    path = tmp_path / "app.kgl"
    threads, per_thread = 8, 25
    s = kglite.open_session(str(path), durable=level)
    errors: list[BaseException] = []
    start = threading.Barrier(threads)

    def work(t: int) -> None:
        try:
            start.wait()
            for i in range(per_thread):
                s.execute("CREATE (:N {id: $id})", params={"id": t * per_thread + i})
        except BaseException as exc:  # noqa: BLE001 - reported below
            errors.append(exc)

    pool = [threading.Thread(target=work, args=(t,)) for t in range(threads)]
    for thread in pool:
        thread.start()
    for thread in pool:
        thread.join(timeout=90)
    assert not errors, errors
    assert _ids(s) == list(range(threads * per_thread))
    s = None  # dropped without close(): the reopen must replay everything from the log
    with kglite.open_session(str(path), durable=level) as reopened:
        assert _ids(reopened) == list(range(threads * per_thread))


def test_readers_never_see_a_partial_write(tmp_path):
    """Each write creates two nodes at once; a count that is ever odd is a torn read."""
    s = kglite.open_session(str(tmp_path / "app.kgl"), durable="full")
    stop = threading.Event()
    seen_odd: list[int] = []
    errors: list[BaseException] = []
    reads = [0]

    def reader() -> None:
        try:
            while not stop.is_set():
                n = s.cypher("MATCH (p:Pair) RETURN count(p) AS n").to_list()[0]["n"]
                reads[0] += 1
                if n % 2:
                    seen_odd.append(n)
        except BaseException as exc:  # noqa: BLE001
            errors.append(exc)

    readers = [threading.Thread(target=reader) for _ in range(3)]
    for thread in readers:
        thread.start()
    for i in range(60):
        s.execute("CREATE (:Pair {k: $k}), (:Pair {k: $k})", params={"k": i})
    stop.set()
    for thread in readers:
        thread.join(timeout=30)
    assert not errors, errors
    assert reads[0] > 0, "the readers must have run during the writes"
    assert not seen_odd, f"torn reads: {seen_odd[:5]}"
    assert s.cypher("MATCH (p:Pair) RETURN count(p) AS n").to_list()[0]["n"] == 120
    s.close()


def test_a_session_can_be_shared_while_one_thread_holds_a_transaction(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="normal") as s:
        tx = s.begin()
        tx.cypher("CREATE (:N {id: 1})")
        s.execute("CREATE (:N {id: 2})")  # no borrow held: another writer is not refused
        assert _ids(s) == [2]  # the open transaction's write is not visible yet
        tx.rollback()


# --- lifecycle --------------------------------------------------------------


def test_a_second_process_is_refused_with_the_error_class_kglite_open_raises(tmp_path):
    path = tmp_path / "app.kgl"
    s = kglite.open_session(str(path), durable="full")
    try:
        for opener in (
            "kglite.open_session(path, durable='full')",
            "kglite.open(path, durable='full')",
        ):
            result = _child(
                f"""
                try:
                    {opener}
                except kglite.WriterLeaseHeldError as exc:
                    print("held", exc.holder["pid"] == {os.getpid()})
                    sys.exit(0)
                sys.exit(3)
                """,
                path,
            )
            assert result.returncode == 0, (opener, result.stdout, result.stderr)
            assert "held True" in result.stdout
    finally:
        s.close()
    # The lease is released: the same child now opens it.
    result = _child("kglite.open_session(path, durable='full').close()", path)
    assert result.returncode == 0, result.stderr


def test_a_second_open_in_this_process_is_refused_until_close(tmp_path):
    path = str(tmp_path / "app.kgl")
    first = kglite.open_session(path, durable="normal")
    with pytest.raises(kglite.WriterLeaseHeldError):
        kglite.open_session(path, durable="normal")
    first.close()
    kglite.open_session(path, durable="normal").close()


def test_close_checkpoints_and_every_write_afterwards_raises(tmp_path):
    path = tmp_path / "app.kgl"
    s = kglite.open_session(str(path), durable="full")
    s.execute("CREATE (:N {id: 1})")
    tx = s.begin()
    tx.cypher("CREATE (:N {id: 2})")
    assert not s.closed
    s.close()
    assert s.closed
    assert path.exists(), "close() checkpoints"

    with pytest.raises(kglite.FileIoError, match="closed"):
        s.execute("CREATE (:N {id: 3})")
    with pytest.raises(kglite.FileIoError, match="closed"):
        s.begin()
    with pytest.raises(kglite.FileIoError, match="closed"):
        tx.commit()
    with pytest.raises(kglite.FileIoError, match="closed"):
        s.save()
    with pytest.raises(kglite.FileIoError, match="closed"):
        s.sync()
    s.close()  # idempotent

    assert _ids(kglite.load(str(path))) == [1], "neither the late write nor the open transaction reached disk"


def test_the_context_manager_checkpoints_on_clean_exit_only_when_asked(tmp_path):
    path = tmp_path / "app.kgl"
    with kglite.open_session(str(path), durable="off") as s:
        s.execute("CREATE (:N {id: 1})")
    assert s.closed
    assert _ids(kglite.load(str(path))) == [1]

    with pytest.raises(RuntimeError, match="boom"):
        with kglite.open_session(str(path), durable="off") as s:
            s.execute("CREATE (:N {id: 2})")
            raise RuntimeError("boom")
    assert _ids(kglite.load(str(path))) == [1], "an exception skips the checkpoint"
    assert s.closed


def test_save_sync_and_backup(tmp_path):
    path = tmp_path / "app.kgl"
    with kglite.open_session(str(path), durable="normal") as s:
        s.execute("CREATE (:N {id: 1})")
        s.sync()
        s.save()
        assert _ids(kglite.load(str(path))) == [1]
        s.execute("CREATE (:N {id: 2})")
        report = s.backup(str(tmp_path / "copy.kgl"))
        assert report["nodes"] == 2
        with pytest.raises(ValueError, match="backup"):
            s.save(str(tmp_path / "elsewhere.kgl"))
        s.save(str(path))  # naming its own checkpoint is allowed
    assert _ids(kglite.load(str(tmp_path / "copy.kgl"))) == [1, 2]


def test_properties(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="normal") as s:
        assert (s.path, s.durability, s.closed) == (str(tmp_path / "app.kgl"), "normal", False)
    legacy = kglite.open_session(str(tmp_path / "app.kgl"))  # the read-only load form
    assert (legacy.durability, legacy.path, legacy.closed) == (None, None, False)
    with pytest.raises(kglite.NotDurableError):
        legacy.begin()
    with pytest.raises(kglite.NotDurableError):
        legacy.close()


def test_a_read_after_close_still_answers_from_the_last_state(tmp_path):
    s = kglite.open_session(str(tmp_path / "app.kgl"), durable="normal")
    s.execute("CREATE (:N {id: 1})")
    s.close()
    assert _ids(s) == [1]


# --- transactions -----------------------------------------------------------


def test_a_transaction_commits_as_one_unit_and_survives_a_crash(tmp_path):
    path = tmp_path / "app.kgl"
    result = _child(
        """
        s = kglite.open_session(path, durable="full")
        with s.begin() as tx:
            tx.cypher("CREATE (:N {id: 1})")
            tx.cypher("CREATE (:N {id: 2})")
            assert s.cypher("MATCH (n:N) RETURN count(n) AS c").to_list()[0]["c"] == 0
        tx = s.begin()
        tx.cypher("CREATE (:N {id: 99})")
        tx.rollback()
        os._exit(0)
        """,
        path,
    )
    assert result.returncode == 0, result.stderr
    with kglite.open_session(str(path), durable="full") as s:
        assert _ids(s) == [1, 2]


def test_an_optimistic_conflict_raises_transaction_conflict(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="normal") as s:
        first, second = s.begin(), s.begin()
        first.cypher("CREATE (:N {id: 1})")
        second.cypher("CREATE (:N {id: 2})")
        first.commit()
        with pytest.raises(kglite.TransactionConflictError):
            second.commit()
        assert _ids(s) == [1]


def test_a_finished_transaction_cannot_be_reused(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="off") as s:
        tx = s.begin()
        tx.commit()
        with pytest.raises(kglite.ArgumentError):
            tx.cypher("RETURN 1")
        with pytest.raises(kglite.ArgumentError):
            tx.commit()


# --- durable="off" ----------------------------------------------------------


def test_off_keeps_no_log_and_relies_on_close(tmp_path):
    path = tmp_path / "app.kgl"
    s = kglite.open_session(str(path), durable="off")
    s.execute("CREATE (:N {id: 1})")
    assert not (tmp_path / "app.kgl-wal").exists()
    with pytest.raises(kglite.NotDurableError):
        s.sync()
    s.save()
    assert _ids(kglite.load(str(path))) == [1]
    s.execute("CREATE (:N {id: 2})")
    s.close()
    assert _ids(kglite.load(str(path))) == [1, 2]


def test_off_loses_unsaved_writes_on_a_crash(tmp_path):
    path = tmp_path / "app.kgl"
    result = _child(
        """
        s = kglite.open_session(path, durable="off")
        s.execute("CREATE (:N {id: 1})")
        s.save()
        s.execute("CREATE (:N {id: 2})")
        os._exit(0)
        """,
        path,
    )
    assert result.returncode == 0, result.stderr
    with kglite.open_session(str(path), durable="off") as s:
        assert _ids(s) == [1]


# --- open errors ------------------------------------------------------------


def test_open_errors_match_kglite_open(tmp_path):
    with pytest.raises(ValueError, match="durability"):
        kglite.open_session(str(tmp_path / "a.kgl"), durable="sometimes")
    with pytest.raises(kglite.ArgumentError):
        kglite.open_session(str(tmp_path / "b.kgl"), durable="off", storage="bogus")
    with pytest.raises(ValueError, match="disk"):
        kglite.open_session(str(tmp_path / "c"), durable="full", storage="disk")
    with pytest.raises(ValueError, match="read-only load form"):
        kglite.open_session(str(tmp_path / "d.kgl"), durable="off", max_load_mb=10)
    with pytest.raises(ValueError, match="durable"):
        kglite.open_session(str(tmp_path / "e.kgl"), auto_checkpoint_wal_mib=4)


def test_off_refuses_to_open_over_an_unreplayed_log(tmp_path):
    path = _crashed_graph(tmp_path, "full")
    with pytest.raises(ValueError, match="holds commits"):
        kglite.open_session(str(path), durable="off")
    with pytest.raises(ValueError, match="holds commits"):
        kglite.open(str(path), durable="off")


def test_a_corrupt_checkpoint_is_not_a_value_error(tmp_path):
    path = tmp_path / "app.kgl"
    path.write_bytes(b"not a graph at all")
    with pytest.raises(Exception) as session_error:
        kglite.open_session(str(path), durable="full")
    with pytest.raises(Exception) as open_error:
        kglite.open(str(path), durable="full")
    assert type(session_error.value) is type(open_error.value)


# --- automatic checkpoint ---------------------------------------------------


def test_the_log_is_folded_into_the_checkpoint_in_the_background(tmp_path):
    path = tmp_path / "app.kgl"
    wal = tmp_path / "app.kgl-wal"
    payload = "x" * 4096
    with kglite.open_session(str(path), durable="normal", auto_checkpoint_wal_mib=1) as s:
        for i in range(600):  # ~2.4 MiB of frames against a 1 MiB bound
            s.execute("CREATE (:N {id: $i, pad: $p})", params={"i": i, "p": payload})
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and not path.exists():
            time.sleep(0.05)
        assert path.exists(), "an online checkpoint must have written the file before close()"
        assert wal.stat().st_size < 2 * 1024 * 1024, "and trimmed the log"
    with kglite.open_session(str(path), durable="normal") as s:
        assert len(_ids(s)) == 600
