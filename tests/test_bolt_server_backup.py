"""`CALL db.backup(<name>)` on kglite-bolt-server: issue #221's acceptance
criteria against a real server process, plus the `--backup-dir` path policy.

AC1 a 10 ms writer keeps getting acks while a backup runs; AC2 the backup
holds a gap-free prefix of the writer's commits matching the returned LSN;
AC3 it opens with no sidecars; AC4 SIGKILL mid-backup leaves no half file;
AC5 a backup over the served graph is refused; AC6 a server restores from it.

POSIX-gated: SIGKILL/SIGINT semantics.
"""

import os
import shutil
import signal
import subprocess
import threading
import time

import pandas as pd
import pytest

import kglite

neo4j = pytest.importorskip("neo4j")

from tests.conftest import (  # noqa: E402
    _BOLT_BINARY,
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [
    pytest.mark.bolt,
    pytest.mark.skipif(os.name != "posix", reason="SIGKILL/SIGINT semantics are POSIX-only"),
]

BIG_NODES = 600_000
SEQ_QUERY = "MATCH (s:Seq) RETURN count(s) AS c, min(s.id) AS lo, max(s.id) AS hi"


@pytest.fixture(autouse=True)
def _require_binary():
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)


def _big_graph(path, n: int = BIG_NODES) -> None:
    """A graph whose backup takes long enough (hundreds of ms in a debug
    build) for a second client action to land inside it."""
    g = kglite.KnowledgeGraph()
    frame = pd.DataFrame(
        {
            "id": list(range(1, n + 1)),
            "title": [f"person-{i}" for i in range(n)],
            "city": [f"city-{i % 977}" for i in range(n)],
            "bio": [f"a long-ish text field number {i} " * 4 for i in range(n)],
        }
    )
    g.add_nodes(frame, "Person", "id", "title")
    g.save(str(path))


def _driver(url):
    return neo4j.GraphDatabase.driver(url, auth=("neo4j", "password"))


def _backup(url, name, query="CALL db.backup($n)"):
    with _driver(url) as driver, driver.session() as session:
        return session.run(query, n=name).single()


def _backup_error(url, name, query="CALL db.backup($n)"):
    with pytest.raises(neo4j.exceptions.Neo4jError) as info:
        _backup(url, name, query)
    return info.value


def _seq(path):
    row = kglite.open(str(path)).cypher(SEQ_QUERY).to_list()[0]
    return row["c"], row["lo"], row["hi"]


def _start(tmp_path, graph_name="graph.kgl", big=False, extra=(), readonly=False, backup_dir=True):
    served_dir = tmp_path / "served"
    served_dir.mkdir(exist_ok=True)
    served = served_dir / graph_name
    (_big_graph if big else _build_bolt_fixture_graph)(served)
    bdir = tmp_path / "backups"
    args = ["--backup-dir", str(bdir)] if backup_dir else []
    proc, url = _spawn_bolt_server(served, readonly=readonly, extra_args=[*args, *extra])
    return proc, url, served, bdir


def _wait_for_partial_file(bdir, final_name, timeout=30.0):
    """Wait until the backup's in-flight temp file exists beside the (not yet
    published) destination; return its name."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if bdir.exists():
            for entry in os.listdir(bdir):
                if entry != final_name and not entry.startswith("."):
                    return entry
                if entry.startswith(".") or entry.endswith(".tmp"):
                    return entry
        time.sleep(0.002)
    raise AssertionError("no in-flight backup file appeared")


class _Writer(threading.Thread):
    """Autocommit one node every ~10 ms, recording each ack's timestamp."""

    def __init__(self, url):
        super().__init__(daemon=True)
        self.url = url
        self.stop = threading.Event()
        self.acks: list[tuple[float, int]] = []
        self.error = None

    def run(self):
        try:
            with _driver(self.url) as driver, driver.session() as session:
                i = 0
                while not self.stop.is_set():
                    i += 1
                    session.run("CREATE (:Seq {id: $i})", i=i).consume()
                    self.acks.append((time.monotonic(), i))
                    time.sleep(0.01)
        except Exception as e:  # noqa: BLE001
            self.error = e

    def max_gap(self, start, end):
        window = [t for t, _ in self.acks if start <= t <= end]
        points = [start, *window, end]
        return max(b - a for a, b in zip(points, points[1:])), len(window)


# ── AC1, AC2, AC3, AC6 ──────────────────────────────────────────────────────


def test_writer_keeps_acking_and_backup_is_a_gap_free_prefix(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, big=True)
    writer = _Writer(url)
    try:
        writer.start()
        time.sleep(0.5)
        t0 = time.monotonic()
        record = _backup(url, "snap.kgl")
        t1 = time.monotonic()
        time.sleep(0.3)
        writer.stop.set()
        writer.join(timeout=10)
        assert writer.error is None, writer.error
        gap, acked_in_window = writer.max_gap(t0, t1)
        print(f"AC1 backup {t1 - t0:.3f}s, writer max gap {gap * 1000:.0f} ms, {acked_in_window} acks inside")
        # AC1: commits flow during the backup. Gap bounded well under the
        # backup's own duration (a serialize-under-lock backup stalls for all of it).
        assert (t1 - t0) > 0.3, "graph too small: the backup finished too fast to prove anything"
        assert acked_in_window >= 10, f"writer starved: {acked_in_window} acks in {t1 - t0:.2f}s"
        assert gap < 0.5, f"writer stalled {gap * 1000:.0f} ms during a {(t1 - t0) * 1000:.0f} ms backup"
        assert record["success"] is True
        assert record["lock_hold_ms"] < 250

        # AC3: one self-contained file, no sidecars beside it.
        assert sorted(os.listdir(bdir)) == ["snap.kgl"]

        # AC2: in a fresh process, ids 1..N contiguous, N matching the lsn.
        out = subprocess.run(
            ["python3", "-I", "-c", _READ_SEQ, str(bdir / "snap.kgl")],
            capture_output=True,
            text=True,
            timeout=60,
        )
        assert out.returncode == 0, out.stderr
        count, lo, hi = (int(x) for x in out.stdout.split())
        assert (count, lo) == (hi, 1), f"not a contiguous prefix: count={count} lo={lo} hi={hi}"
        assert record["lsn"] == hi, f"lsn {record['lsn']} does not match last commit in file {hi}"
        assert record["nodes"] == BIG_NODES + count
        assert any(i > hi for _, i in writer.acks), "writer did not outlive the snapshot point"
        assert record["path"] == str((bdir.resolve() / "snap.kgl"))
    finally:
        writer.stop.set()
        _teardown_bolt_server(proc)


_READ_SEQ = (
    "import sys, kglite;"
    "r = kglite.open(sys.argv[1]).cypher(" + repr(SEQ_QUERY) + ").to_list()[0];"
    "print(r['c'], r['lo'], r['hi'])"
)


def test_server_restores_from_a_backup(tmp_path):
    proc, url, _served, bdir = _start(tmp_path)
    try:
        with _driver(url) as driver, driver.session() as session:
            session.run("CREATE (:Person {id: 77, title: 'Zed', city: 'Tromso'})").consume()
        _backup(url, "restore.kgl")
    finally:
        _teardown_bolt_server(proc)
    restored = tmp_path / "restored.kgl"
    shutil.copyfile(bdir / "restore.kgl", restored)
    proc2, url2 = _spawn_bolt_server(restored)
    try:
        with _driver(url2) as driver, driver.session() as session:
            titles = {r["t"] for r in session.run("MATCH (p:Person) RETURN p.title AS t")}
        assert titles == {"Alice", "Bob", "Carol", "Dave", "Zed"}
    finally:
        _teardown_bolt_server(proc2)


# ── AC4 ─────────────────────────────────────────────────────────────────────


def test_sigkill_mid_backup_leaves_no_destination(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, big=True)
    try:
        result = {}
        t = threading.Thread(target=lambda: result.update(r=_try(lambda: _backup(url, "k.kgl"))), daemon=True)
        t.start()
        _wait_for_partial_file(bdir, "k.kgl")
        proc.send_signal(signal.SIGKILL)
        proc.wait(timeout=10)
        t.join(timeout=20)
        assert "r" in result and isinstance(result["r"], Exception), "backup must not have completed"
    finally:
        _teardown_bolt_server(proc)
    assert not (bdir / "k.kgl").exists(), "a killed backup must not publish the destination"


def test_sigkill_mid_backup_keeps_the_previous_complete_backup(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, big=True)
    try:
        _backup(url, "k.kgl")
        before = (bdir / "k.kgl").read_bytes()
        with _driver(url) as driver, driver.session() as session:
            session.run("CREATE (:Seq {id: 1})").consume()
        t = threading.Thread(target=lambda: _try(lambda: _backup(url, "k.kgl")), daemon=True)
        t.start()
        _wait_for_partial_file(bdir, "k.kgl")
        proc.send_signal(signal.SIGKILL)
        proc.wait(timeout=10)
        t.join(timeout=20)
    finally:
        _teardown_bolt_server(proc)
    assert (bdir / "k.kgl").read_bytes() == before
    assert kglite.open(str(bdir / "k.kgl")).cypher("MATCH (p:Person) RETURN count(p) AS c").scalar() == BIG_NODES


def _try(fn):
    try:
        return fn()
    except Exception as e:  # noqa: BLE001
        return e


# ── AC5 and the other refusals ──────────────────────────────────────────────


def test_backup_over_the_served_graph_is_refused(tmp_path):
    served_dir = tmp_path / "bk"
    served_dir.mkdir()
    served = served_dir / "graph.kgl"
    _build_bolt_fixture_graph(served)
    proc, url = _spawn_bolt_server(served, extra_args=["--backup-dir", str(served_dir)])
    try:
        before = served.read_bytes()
        err = _backup_error(url, "graph.kgl")
        assert err.code == "Neo.ClientError.Security.Forbidden"
        assert "checkpoint" in err.message
        assert served.read_bytes() == before
    finally:
        _teardown_bolt_server(proc)


def test_backup_is_disabled_without_backup_dir(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, backup_dir=False)
    try:
        err = _backup_error(url, "x.kgl")
        assert err.code == "Neo.ClientError.Security.Forbidden"
        assert "--backup-dir" in err.message
        assert not bdir.exists()
    finally:
        _teardown_bolt_server(proc)


@pytest.mark.parametrize(
    "name",
    ["../escaped.kgl", "sub/x.kgl", "ABSOLUTE", "..\\x.kgl", "a/../../b.kgl", "", ".."],
)
def test_backup_names_that_leave_the_dir_are_refused(tmp_path, name):
    proc, url, _served, bdir = _start(tmp_path)
    absolute = tmp_path / "abs-escaped.kgl"
    name = str(absolute) if name == "ABSOLUTE" else name
    try:
        err = _backup_error(url, name)
        assert err.code == "Neo.ClientError.Security.Forbidden", err
        assert "bare file name" in err.message
        assert not (tmp_path / "escaped.kgl").exists()
        assert not absolute.exists()
        assert os.listdir(bdir) == []
    finally:
        _teardown_bolt_server(proc)


def test_backup_literal_form_and_yield_subset(tmp_path):
    proc, url, _served, bdir = _start(tmp_path)
    try:
        record = _backup(url, None, "CALL db.backup('lit.kgl') YIELD path, bytes, lsn")
        assert record.keys() == ["path", "bytes", "lsn"]
        assert record["bytes"] == (bdir / "lit.kgl").stat().st_size
        assert record["lsn"] is not None  # default --durability normal logs
    finally:
        _teardown_bolt_server(proc)


def test_backup_lsn_is_null_without_a_log(tmp_path):
    proc, url, _served, _bdir = _start(tmp_path, extra=["--durability", "off"])
    try:
        assert _backup(url, "off.kgl")["lsn"] is None
    finally:
        _teardown_bolt_server(proc)


def test_backup_is_allowed_on_a_readonly_server(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, readonly=True)
    try:
        assert _backup(url, "ro.kgl")["success"] is True
        assert (bdir / "ro.kgl").is_file()
    finally:
        _teardown_bolt_server(proc)


def test_backup_inside_an_explicit_transaction_is_refused(tmp_path):
    proc, url, _served, bdir = _start(tmp_path)
    try:
        with _driver(url) as driver, driver.session() as session:
            tx = session.begin_transaction()
            with pytest.raises(neo4j.exceptions.Neo4jError) as info:
                tx.run("CALL db.backup('tx.kgl')").consume()
            assert "explicit transaction" in info.value.message
            tx.close()
        assert not (bdir / "tx.kgl").exists()
    finally:
        _teardown_bolt_server(proc)


def test_second_concurrent_backup_is_refused_not_queued(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, big=True)
    try:
        first = {}
        t = threading.Thread(target=lambda: first.update(r=_try(lambda: _backup(url, "one.kgl"))), daemon=True)
        t.start()
        _wait_for_partial_file(bdir, "one.kgl")
        err = _backup_error(url, "two.kgl")
        assert "already in progress" in err.message
        t.join(timeout=60)
        assert not isinstance(first["r"], Exception), first["r"]
        assert not (bdir / "two.kgl").exists()
        # the slot is free again
        assert _backup(url, "three.kgl")["success"] is True
    finally:
        _teardown_bolt_server(proc)


# ── startup policy ──────────────────────────────────────────────────────────


def test_allow_any_path_is_refused_at_startup_under_auth_none(tmp_path):
    fixture = tmp_path / "f.kgl"
    _build_bolt_fixture_graph(fixture)
    out = subprocess.run(
        [str(_BOLT_BINARY), "--graph", str(fixture), "--port", "0", "--backup-allow-any-path"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert out.returncode != 0
    assert "--backup-allow-any-path" in out.stderr and "--auth none" in out.stderr


def test_allow_any_path_with_auth_takes_an_absolute_path(tmp_path):
    fixture = tmp_path / "f.kgl"
    _build_bolt_fixture_graph(fixture)
    proc, url = _spawn_bolt_server(
        fixture,
        extra_args=["--auth", "basic", "--auth-user", "u", "--auth-pass", "p", "--backup-allow-any-path"],
    )
    try:
        target = tmp_path / "elsewhere" / "abs.kgl"
        target.parent.mkdir()
        with neo4j.GraphDatabase.driver(url, auth=("u", "p")) as driver, driver.session() as session:
            record = session.run("CALL db.backup($n)", n=str(target)).single()
        assert record["path"] == str(target) and target.is_file()
    finally:
        _teardown_bolt_server(proc)


# ── Scheduled backups: --backup-interval / --backup-keep ────────────────────


def _scheduled(bdir):
    return sorted(p.name for p in bdir.glob("graph-*.kgl")) if bdir.exists() else []


def _wait_for(predicate, timeout=20.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise AssertionError("condition not reached in time")


def _create_person(url, pid):
    with _driver(url) as driver, driver.session() as session:
        session.run("CREATE (:Person {id: $i, title: 't', city: 'c'})", i=pid).consume()


def test_schedule_flags_without_backup_dir_are_refused_at_startup(tmp_path):
    served = tmp_path / "graph.kgl"
    _build_bolt_fixture_graph(served)
    for flags in (["--backup-interval", "5"], ["--backup-keep", "2"]):
        result = subprocess.run(
            [str(_BOLT_BINARY), "--graph", str(served), "--port", "0", *flags],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode != 0
        assert "--backup-dir" in result.stderr


def test_interval_fires_keep_prunes_and_foreign_files_survive(tmp_path):
    served_dir = tmp_path / "served"
    served_dir.mkdir()
    served = served_dir / "graph.kgl"
    _build_bolt_fixture_graph(served)
    bdir = tmp_path / "backups"
    bdir.mkdir()
    (bdir / "manual.kgl").write_bytes(b"manual backup")
    (bdir / "notes.txt").write_text("foreign", encoding="utf-8")
    (bdir / "other-20240101T000000Z.kgl").write_bytes(b"other stem")
    proc, url = _spawn_bolt_server(
        served, extra_args=["--backup-dir", str(bdir), "--backup-interval", "1", "--backup-keep", "2"]
    )
    try:
        # Each pass changes the graph, so no tick is skipped as unchanged.
        seen = set()
        pid = 100
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline and len(seen) < 4:
            _create_person(url, pid)
            pid += 1
            seen.update(_scheduled(bdir))
            time.sleep(0.5)
        assert len(seen) >= 4, f"interval never produced enough backups: {seen}"
        final = _scheduled(bdir)
        assert len(final) <= 2, f"--backup-keep 2 left {final}"
        assert set(final) <= seen
        assert (bdir / "manual.kgl").read_bytes() == b"manual backup"
        assert (bdir / "notes.txt").read_text(encoding="utf-8") == "foreign"
        assert (bdir / "other-20240101T000000Z.kgl").read_bytes() == b"other stem"
    finally:
        _teardown_bolt_server(proc)
    newest = bdir / _scheduled(bdir)[-1]
    assert _seq_or_people(newest) >= 5


def _seq_or_people(path):
    return kglite.open(str(path)).cypher("MATCH (p:Person) RETURN count(p) AS c").to_list()[0]["c"]


def test_unchanged_graph_is_not_backed_up_again(tmp_path):
    proc, url, _served, bdir = _start(tmp_path, extra=["--backup-interval", "1"])
    try:
        _create_person(url, 500)
        first = _wait_for(lambda: _scheduled(bdir))
        time.sleep(4)  # several ticks with no write in between
        assert _scheduled(bdir) == first
        _create_person(url, 501)
        _wait_for(lambda: len(_scheduled(bdir)) > len(first))
    finally:
        _teardown_bolt_server(proc)
