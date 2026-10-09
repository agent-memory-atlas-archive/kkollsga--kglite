"""RESET and a dropped connection cancel the query the session is running.

`boltr` reads one message at a time and does not touch the socket while a RUN
executes. The server therefore reads the input itself (see `pump.rs`) and
cancels the running query when it sees a RESET or the peer disappears. Raw
Bolt is used where the exact message order matters (RUN's FAILURE, then RESET's
SUCCESS); the official driver covers a real connection drop.
"""

import os
import time

import pandas as pd
import pytest

import kglite
from tests.bolt_raw import (
    SIG_BEGIN,
    SIG_FAILURE,
    SIG_GOODBYE,
    SIG_RESET,
    SIG_RUN,
    SIG_SUCCESS,
    RawBolt,
)
from tests.conftest import (
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt, pytest.mark.skipif(os.name != "posix", reason="POSIX process control")]

N = 500
AUTH = ("neo4j", "password")
# N^3 candidates: minutes of work uncancelled, but every step polls the cancel flag.
TAIL = "MATCH (b:P), (c:P) WHERE b.id + c.id + a.id = -1 RETURN count(*) AS c"
SLOW_READ = "MATCH (a:P), (b:P), (c:P) WHERE a.id + b.id + c.id = -1 RETURN count(*) AS c"
# Writes every node first, then spends minutes in the tail.
SLOW_WRITE = f"MATCH (a:P) SET a.flag = 1 WITH a {TAIL}"
PROMPT = 10.0


@pytest.fixture(autouse=True)
def _require_binary():
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)


@pytest.fixture
def server(tmp_path):
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": list(range(N)), "title": [f"p{i}" for i in range(N)]}), "P", "id", "title")
    path = tmp_path / "cancel.kgl"
    g.save(str(path))
    # The engine's default work ceiling would end the slow queries by itself; raise
    # it so only cancellation can.
    proc, url = _spawn_bolt_server(path, extra_args=["--max-work-units", "1000000000000"])
    host, port = url.removeprefix("bolt://").split(":")
    yield url, (host, int(port))
    _teardown_bolt_server(proc)


def _raw(addr) -> RawBolt:
    conn = RawBolt(*addr, timeout=PROMPT * 3)
    assert conn.hello() == SIG_SUCCESS
    assert conn.logon(*AUTH) == SIG_SUCCESS
    return conn


def _prompt_write(driver) -> float:
    """Seconds for a fresh write to commit; fails if it waits on a stuck writer."""
    started = time.monotonic()
    with driver.session() as s:
        s.run("CREATE (:Probe {id: 1})").consume()
    return time.monotonic() - started


def _flagged(driver) -> int:
    with driver.session() as s:
        return s.run("MATCH (a:P) WHERE a.flag = 1 RETURN count(a) AS c").single()["c"]


def test_reset_interrupts_a_running_auto_commit_query(server):
    _, addr = server
    with _raw(addr) as conn:
        conn.send(SIG_RUN, SLOW_READ, {}, {})
        time.sleep(0.5)
        started = time.monotonic()
        conn.send(SIG_RESET)
        assert conn.recv()[0] == SIG_FAILURE, "the interrupted RUN fails"
        assert conn.recv()[0] == SIG_SUCCESS, "RESET itself succeeds"
        assert time.monotonic() - started < PROMPT
        # The session is usable afterwards.
        assert conn.request(SIG_RUN, "RETURN 1 AS x", {}, {}) == SIG_SUCCESS


def test_the_interrupted_run_reports_a_terminated_transaction(server):
    _, addr = server
    with _raw(addr) as conn:
        conn.send(SIG_RUN, SLOW_READ, {}, {})
        time.sleep(0.5)
        conn.send(SIG_RESET)
        sig, body = conn.recv()
        assert sig == SIG_FAILURE
        assert b"Transaction.Terminated" in body, body


def test_reset_cancels_an_auto_commit_write_before_it_publishes(server):
    url, addr = server
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with _raw(addr) as conn:
            conn.send(SIG_RUN, SLOW_WRITE, {}, {})
            time.sleep(1.0)
            conn.send(SIG_RESET)
            assert conn.recv()[0] == SIG_FAILURE
            assert conn.recv()[0] == SIG_SUCCESS
        assert _flagged(driver) == 0, "a cancelled auto-commit write must not be committed"
        assert _prompt_write(driver) < PROMPT, "the writer slot must be free"


def test_reset_rolls_back_a_cancelled_statement_in_an_explicit_transaction(server):
    url, addr = server
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with _raw(addr) as conn:
            assert conn.request(SIG_BEGIN, {}) == SIG_SUCCESS
            conn.send(SIG_RUN, SLOW_WRITE, {}, {})
            time.sleep(1.0)
            conn.send(SIG_RESET)
            assert conn.recv()[0] == SIG_FAILURE
            assert conn.recv()[0] == SIG_SUCCESS
            # The transaction is gone: a new one begins and the writer slot is free.
            assert conn.request(SIG_BEGIN, {}) == SIG_SUCCESS
        assert _flagged(driver) == 0
        assert _prompt_write(driver) < PROMPT


def test_a_dropped_connection_cancels_the_running_write_and_frees_the_writer(server):
    url, addr = server
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        conn = _raw(addr)
        conn.send(SIG_RUN, SLOW_WRITE, {}, {})
        time.sleep(1.0)
        conn.close()  # no GOODBYE: the peer simply vanishes
        assert _prompt_write(driver) < PROMPT, "the abandoned write must not hold the writer slot"
        assert _flagged(driver) == 0


def test_goodbye_after_a_pipelined_write_still_commits_it(server):
    """GOODBYE is orderly, not an interrupt: the statement it follows completes."""
    url, addr = server
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        conn = _raw(addr)
        conn.send(SIG_RUN, "CREATE (:Kept {id: 7})", {}, {})
        conn.send(SIG_GOODBYE)
        conn.sock.shutdown(1)
        time.sleep(1.0)
        conn.close()
        with driver.session() as s:
            assert s.run("MATCH (k:Kept) RETURN count(k) AS c").single()["c"] == 1


def test_reset_with_nothing_running_is_a_plain_success(server):
    _, addr = server
    with _raw(addr) as conn:
        assert conn.request(SIG_RESET) == SIG_SUCCESS
        assert conn.request(SIG_RUN, "RETURN 1 AS x", {}, {}) == SIG_SUCCESS
