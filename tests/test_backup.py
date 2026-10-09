"""KnowledgeGraph.backup / Session.backup: consistent single-file copies."""

import os
import threading
import time

import pytest

import kglite

BIG = 400_000


def build(n=50, storage=None):
    g = kglite.KnowledgeGraph() if storage is None else kglite.KnowledgeGraph(storage=storage)
    g.cypher(f"UNWIND range(1, {n}) AS i CREATE (:Doc {{id: i, name: 'doc' + toString(i)}})")
    g.cypher("MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:LINKS {w: 3}]->(b)")
    return g


def rows(g):
    return list(g.cypher("MATCH (n:Doc) RETURN n.id AS id, n.name AS name ORDER BY id"))


def sidecars(path):
    return [
        p
        for p in os.listdir(os.path.dirname(path))
        if p.startswith(os.path.basename(path)) and p != os.path.basename(path)
    ]


@pytest.mark.parametrize("storage", [None, "mapped"])
def test_round_trip(tmp_path, storage):
    g = build(storage=storage)
    dest = str(tmp_path / "b.kgl")
    report = g.backup(dest)
    assert report["path"] == dest
    assert report["nodes"] == 50 and report["relationships"] == 1
    assert report["lsn"] is None
    assert report["bytes"] == os.path.getsize(dest) > 0
    assert set(report) == {
        "path",
        "bytes",
        "nodes",
        "relationships",
        "graph_version",
        "lsn",
        "lock_hold_ms",
        "elapsed_ms",
        "prepared_copy",
    }
    assert report["prepared_copy"] is False
    restored = kglite.load(dest)
    assert rows(restored) == rows(g)
    rel = restored.cypher("MATCH ()-[r:LINKS]->() RETURN r.w AS w")
    assert [r["w"] for r in rel] == [3]
    assert sidecars(dest) == []


def test_backup_accepts_pathlike_and_leaves_save_target(tmp_path):
    from pathlib import Path

    g = build()
    g.backup(Path(tmp_path / "b.kgl"))
    with pytest.raises(ValueError, match="path"):
        g.save()  # no remembered path: backup did not become the save target


def test_overwrite_replaces_atomically(tmp_path):
    g = build(10)
    dest = str(tmp_path / "b.kgl")
    g.backup(dest)
    g.cypher("CREATE (:Doc {id: 99, name: 'late'})")
    g.backup(dest)
    assert len(rows(kglite.load(dest))) == 11
    assert [p for p in os.listdir(tmp_path)] == ["b.kgl"]


def test_alias_refused_for_loaded_graph(tmp_path):
    src = str(tmp_path / "live.kgl")
    build(5).save(src)
    g = kglite.load(src)
    before = os.path.getsize(src)
    with pytest.raises(ValueError, match="live graph's checkpoint"):
        g.backup(src)
    with pytest.raises(ValueError, match="live graph's checkpoint"):
        g.backup(str(tmp_path / "." / "live.kgl"))
    assert os.path.getsize(src) == before
    with pytest.raises(ValueError, match="live graph's checkpoint"):
        g.session().backup(src)


@pytest.mark.parametrize("level", ["full", "normal"])
def test_durable_lsn_and_no_sidecars(tmp_path, level):
    live = str(tmp_path / "live.kgl")
    g = kglite.open(live, durable=level)
    for i in range(1, 4):
        g.cypher(f"CREATE (:Doc {{id: {i}, name: 'd{i}'}})")
    dest = str(tmp_path / "bk" / "b.kgl")
    os.mkdir(os.path.dirname(dest))
    report = g.backup(dest)
    assert report["lsn"] == 3
    assert report["nodes"] == 3
    assert sidecars(dest) == []
    with pytest.raises(ValueError, match="live graph's checkpoint"):
        g.backup(live)
    # the live graph keeps logging after the backup
    g.cypher("CREATE (:Doc {id: 4, name: 'd4'})")
    assert g.backup(str(tmp_path / "bk" / "c.kgl"))["lsn"] == 4
    reopened = kglite.open(dest, durable=level)
    assert [r["id"] for r in rows(reopened)] == [1, 2, 3]
    reopened.cypher("CREATE (:Doc {id: 5, name: 'd5'})")
    assert len(rows(reopened)) == 4
    del reopened


def test_stray_wal_beside_destination_refused(tmp_path):
    dest = tmp_path / "b.kgl"
    d = kglite.open(str(dest), durable="full")
    d.cypher("CREATE (:Doc {id: 1, name: 'x'})")
    del d  # leaves b.kgl-wal holding an un-checkpointed commit
    assert (tmp_path / "b.kgl-wal").exists()
    with pytest.raises(ValueError):
        build(3).backup(str(dest))


def test_disk_mode_refused(tmp_path):
    g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "disk"))
    g.cypher("CREATE (:Doc {id: 1, name: 'x'})")
    dest = tmp_path / "b.kgl"
    with pytest.raises(ValueError, match="disk"):
        g.backup(str(dest))
    assert not dest.exists()


def test_unwritable_destination_is_file_io_error(tmp_path):
    with pytest.raises(kglite.FileIoError):
        build(3).backup(str(tmp_path / "missing" / "b.kgl"))


def test_graph_usable_from_another_thread_during_backup(tmp_path):
    """The PyO3 borrow is released before the serialize (no 'Already borrowed')."""
    g = kglite.KnowledgeGraph()
    g.cypher(f"UNWIND range(1, {BIG}) AS i CREATE (:Doc {{id: i, name: 'doc' + toString(i)}})")
    dest = str(tmp_path / "b.kgl")
    window = {}

    def backup():
        window["start"] = time.monotonic()
        window["report"] = g.backup(dest)
        window["end"] = time.monotonic()

    t = threading.Thread(target=backup)
    t.start()
    while "start" not in window:
        time.sleep(0.001)
    time.sleep(0.02)  # let the call get past the snapshot into the serialize
    ok, errors = 0, []
    while t.is_alive():
        try:
            g.cypher("MATCH (n:Doc {id: 7}) RETURN n.id AS id")
            ok += 1
        except Exception as e:  # noqa: BLE001 - the failure under test
            errors.append(repr(e))
    t.join()
    assert errors == [], errors[:3]
    assert ok > 0, "backup finished before any concurrent read ran; enlarge BIG"
    assert window["report"]["nodes"] == BIG


def test_session_writer_makes_progress_during_backup(tmp_path):
    g = kglite.KnowledgeGraph()
    g.cypher(f"UNWIND range(1, {BIG}) AS i CREATE (:Doc {{id: i, name: 'doc' + toString(i)}})")
    s = g.session()
    dest = str(tmp_path / "b.kgl")
    stop = threading.Event()
    stamps = []
    errors = []

    def writer():
        n = BIG
        while not stop.is_set():
            n += 1
            try:
                s.execute(f"CREATE (:Doc {{id: {n}, name: 'w'}})")
            except Exception as e:  # noqa: BLE001
                errors.append(repr(e))
                return
            stamps.append(time.monotonic())

    w = threading.Thread(target=writer)
    w.start()
    while not stamps:
        time.sleep(0.001)
    t0 = time.monotonic()
    report = s.backup(dest)
    t1 = time.monotonic()
    stop.set()
    w.join()
    assert errors == [], errors[:3]
    during = [x for x in stamps if t0 <= x <= t1]
    assert len(during) >= 1, "writer made no progress while the backup ran"
    assert report["lsn"] is None and report["lock_hold_ms"] < report["elapsed_ms"]
    # point-in-time: the file holds a consistent prefix of the writer's inserts
    restored = kglite.load(dest)
    n = restored.cypher("MATCH (n:Doc) RETURN count(n) AS c")[0]["c"]
    assert n == report["nodes"] and BIG <= n <= BIG + len(stamps)
    ids = restored.cypher("MATCH (n:Doc) RETURN max(n.id) AS m")[0]["m"]
    assert ids == n  # gap-free: no commit half-visible
