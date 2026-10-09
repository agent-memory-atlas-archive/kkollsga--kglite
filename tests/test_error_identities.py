"""One identity per failure across the wheel's logged-write, lease and read-only paths.

The binding-parity audit found the same condition spelled differently per
surface. These tests pin the wheel's side of the shared identities in
`KgErrorCode`:

- writer-lease contention is `WriterLeaseHeldError` (a `FileIoError`) with
  `.code == "WriterLeaseHeld"` and a structured `.holder`;
- a write refused by a read-only handle is `ReadOnlyError` (an `ArgumentError`)
  with `.code == "ReadOnly"`;
- `sync()` on a graph with no write-ahead log is `NotDurableError` (a `KgError`
  and a `ValueError`) with `.code == "NotDurable"`;
- a write-ahead-log failure is a `FileIoError` with `.code == "DurabilityFailed"`
  from every logged-write path.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pytest

import kglite

# ─── WriterLeaseHeld ─────────────────────────────────────────────────────────


def test_a_second_open_in_this_process_raises_the_typed_lease_error(tmp_path):
    path = str(tmp_path / "app.kgl")
    first = kglite.open(path)
    try:
        with pytest.raises(kglite.WriterLeaseHeldError) as caught:
            kglite.open(path)
    finally:
        del first
    err = caught.value
    assert isinstance(err, kglite.FileIoError), "an existing `except FileIoError` must still catch it"
    assert isinstance(err, kglite.KgError)
    assert err.code == "WriterLeaseHeld"
    assert kglite.WriterLeaseHeldError.code == "WriterLeaseHeld"
    assert err.holder["pid"] == os.getpid()
    assert err.holder["self"] is True
    assert set(err.holder) == {"pid", "since", "label", "self"}
    # The Python-specific way out is still appended to the message.
    assert "kglite.load(path)" in str(err)


def test_a_holder_in_another_process_is_named_in_the_structured_holder(tmp_path):
    path = str(tmp_path / "app.kgl")
    child = subprocess.Popen(
        [
            sys.executable,
            "-I",
            "-c",
            textwrap.dedent(
                f"""
                import sys, kglite
                g = kglite.open({path!r})
                print("ready", flush=True)
                sys.stdin.readline()
                """
            ),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    try:
        assert child.stdout.readline().strip() == "ready"
        with pytest.raises(kglite.WriterLeaseHeldError) as caught:
            kglite.open(path)
        holder = caught.value.holder
        assert holder["pid"] == child.pid
        assert holder["self"] is False
        assert isinstance(holder["since"], str) and holder["since"]
    finally:
        child.stdin.write("\n")
        child.stdin.flush()
        child.wait(timeout=30)


def test_a_save_as_onto_a_leased_path_raises_the_same_typed_error(tmp_path):
    held = str(tmp_path / "held.kgl")
    other = str(tmp_path / "other.kgl")
    blocker = kglite.open(held)
    g = kglite.open(other)
    try:
        g.cypher("CREATE (:Row {id: 1})")
        with pytest.raises(kglite.WriterLeaseHeldError) as caught:
            g.save(held)
        assert caught.value.code == "WriterLeaseHeld"
        assert caught.value.holder["pid"] == os.getpid()
    finally:
        del g, blocker


def test_an_io_failure_taking_the_lease_is_still_a_plain_file_io_error(tmp_path):
    if os.geteuid() == 0:
        pytest.skip("root ignores directory permissions")
    locked = tmp_path / "readonly"
    locked.mkdir()
    os.chmod(locked, 0o500)
    try:
        with pytest.raises(kglite.FileIoError) as caught:
            kglite.open(str(locked / "app.kgl"))
    finally:
        os.chmod(locked, 0o700)
    assert not isinstance(caught.value, kglite.WriterLeaseHeldError)
    assert caught.value.code == "FileIo"


# ─── NotDurable ──────────────────────────────────────────────────────────────


def test_sync_without_a_log_raises_not_durable_error(tmp_path):
    for graph in (kglite.KnowledgeGraph(), kglite.open(str(tmp_path / "off.kgl"), durable="off")):
        with pytest.raises(kglite.NotDurableError) as caught:
            graph.sync()
        err = caught.value
        assert isinstance(err, kglite.KgError)
        assert isinstance(err, ValueError), "the pre-existing `except ValueError` must still catch it"
        assert err.code == "NotDurable"
        assert kglite.NotDurableError.code == "NotDurable"
        assert "save()" in str(err)


# ─── ReadOnly ────────────────────────────────────────────────────────────────


def test_a_write_on_a_read_only_graph_raises_read_only_error():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Row {id: 1})")
    g.read_only(True)
    with pytest.raises(kglite.ReadOnlyError) as caught:
        g.cypher("CREATE (:Row {id: 2})")
    err = caught.value
    assert isinstance(err, kglite.ArgumentError), "an existing `except ArgumentError` must still catch it"
    assert err.code == "ReadOnly"
    assert kglite.ReadOnlyError.code == "ReadOnly"
    assert g.cypher("MATCH (r:Row) RETURN count(r) AS c").to_list() == [{"c": 1}]


def test_a_write_in_a_read_only_transaction_raises_the_same_error():
    g = kglite.KnowledgeGraph()
    with g.begin_read() as tx:
        with pytest.raises(kglite.ReadOnlyError) as caught:
            tx.cypher("CREATE (:Row {id: 1})")
    assert caught.value.code == "ReadOnly"
    assert isinstance(caught.value, kglite.ArgumentError)


def test_an_unrelated_argument_error_is_not_read_only():
    with pytest.raises(kglite.ArgumentError) as caught:
        kglite.KnowledgeGraph(storage="invalid")
    assert not isinstance(caught.value, kglite.ReadOnlyError)
    assert caught.value.code == "InvalidArgument"


# ─── DurabilityFailed ────────────────────────────────────────────────────────


def _durable(tmp_path):
    return kglite.open(str(tmp_path / "app.kgl"), durable="full")


def test_a_refused_log_append_from_cypher_is_durability_failed(tmp_path):
    g = _durable(tmp_path)
    g.cypher("CREATE (:Row {id: 1})")
    kglite._fail_wal_append(g, True)
    with pytest.raises(kglite.FileIoError) as caught:
        g.cypher("CREATE (:Row {id: 2})")
    assert caught.value.code == "DurabilityFailed"
    # The handle is latched afterwards; that refusal is the same identity.
    kglite._fail_wal_append(g, False)
    with pytest.raises(kglite.FileIoError) as latched:
        g.cypher("CREATE (:Row {id: 3})")
    assert latched.value.code == "DurabilityFailed"
    with pytest.raises(kglite.FileIoError) as saving:
        g.save()
    assert saving.value.code == "DurabilityFailed"


def test_a_refused_log_append_from_a_transaction_commit_is_durability_failed(tmp_path):
    g = _durable(tmp_path)
    tx = g.begin()
    tx.cypher("CREATE (:Row {id: 1})")
    kglite._fail_wal_append(g, True)
    with pytest.raises(kglite.FileIoError) as caught:
        tx.commit()
    assert caught.value.code == "DurabilityFailed"


def test_an_ordinary_io_failure_keeps_the_file_io_code(tmp_path):
    if os.geteuid() == 0:
        pytest.skip("root ignores directory permissions")
    d = tmp_path / "readonly"
    d.mkdir()
    os.chmod(d, 0o500)
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Row {id: 1})")
    try:
        with pytest.raises(kglite.FileIoError) as caught:
            g.save(str(d / "app.kgl"))
    finally:
        os.chmod(d, 0o700)
    assert caught.value.code == "FileIo"
