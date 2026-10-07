"""Writer queue (`--write-concurrency queue`, the default) over real Bolt.

A write-mode transaction takes the process-wide writer slot at BEGIN and holds
it until COMMIT / ROLLBACK / disconnect, so concurrent writers wait instead of
conflicting at COMMIT. These tests drive that through the official `neo4j`
driver; the backend-level cases (FIFO order, reclaim races) live in the Rust
tests next to the implementation.

Raw `begin_transaction` is used wherever a *retry would hide a failure*:
`execute_write` silently retries a retriable error, so a conflict there looks
like success. Raw transactions have no retry, so any conflict raises.
"""

from concurrent.futures import ThreadPoolExecutor
import threading
import time

import pytest

from tests.conftest import (
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt]

AUTH = ("neo4j", "password")
WAIT_TIMEOUT_CODE = "Neo.TransientError.Transaction.LockAcquisitionTimeout"


@pytest.fixture
def make_server(tmp_path):
    """Factory: spawn a server with extra flags, tear every one down."""
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    spawned = []

    def spawn(*extra_args: str) -> str:
        fixture = tmp_path / f"fixture{len(spawned)}.kgl"
        _build_bolt_fixture_graph(fixture)
        proc, url = _spawn_bolt_server(fixture, extra_args=list(extra_args))
        spawned.append(proc)
        return url

    yield spawn
    for proc in spawned:
        _teardown_bolt_server(proc)


def _count(session, where: str = "") -> int:
    clause = f"WHERE {where}" if where else ""
    return session.run(f"MATCH (n:Person) {clause} RETURN count(n) AS c").single()["c"]


def test_four_writers_on_disjoint_keys_never_conflict(make_server):
    """Raw transactions, no retry: any conflict would raise out of the worker."""
    url = make_server()
    writers, rounds = 4, 15

    def worker(driver, w: int) -> None:
        with driver.session() as session:
            for r in range(rounds):
                tx = session.begin_transaction()
                tx.run("CREATE (:Person {id: $id, title: 'Q'})", id=10_000 + w * 100 + r).consume()
                tx.commit()

    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with ThreadPoolExecutor(max_workers=writers) as pool:
            for f in [pool.submit(worker, driver, w) for w in range(writers)]:
                f.result()  # re-raises a conflict
        with driver.session() as session:
            assert _count(session, "n.title = 'Q'") == writers * rounds


def test_four_writers_on_one_key_lose_no_increment_and_run_each_unit_once(make_server):
    """Overlapping keys through `execute_write`: the unit of work runs once per
    commit (no hidden retries) and no increment is lost."""
    url = make_server()
    writers, rounds = 4, 10
    runs = {"n": 0}
    lock = threading.Lock()

    def bump(tx):
        with lock:
            runs["n"] += 1
        tx.run("MATCH (n:Person {title: 'Alice'}) SET n.hits = coalesce(n.hits, 0) + 1").consume()

    def worker(driver) -> None:
        with driver.session() as session:
            for _ in range(rounds):
                session.execute_write(bump)

    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with ThreadPoolExecutor(max_workers=writers) as pool:
            for f in [pool.submit(worker, driver) for _ in range(writers)]:
                f.result()
        with driver.session() as session:
            hits = session.run("MATCH (n:Person {title: 'Alice'}) RETURN n.hits AS h").single()["h"]
    assert hits == writers * rounds, "an acknowledged increment was lost"
    assert runs["n"] == writers * rounds, "the driver retried: a commit conflicted"


def test_reads_proceed_while_a_writer_holds_the_slot(make_server):
    url = make_server()
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with driver.session() as writer:
            tx = writer.begin_transaction()
            tx.run("CREATE (:Person {id: 20001, title: 'Held'})").consume()
            started = time.monotonic()
            with driver.session() as reader:
                # Snapshot isolation unchanged: the uncommitted write is invisible.
                seen = reader.execute_read(
                    lambda t: t.run("MATCH (n:Person {title: 'Held'}) RETURN count(n) AS c").single()["c"]
                )
                auto = _count(reader)
            assert time.monotonic() - started < 5, "a read waited for the writer"
            assert seen == 0 and auto == 4
            tx.commit()
        with driver.session() as session:
            assert _count(session, "n.title = 'Held'") == 1


def test_a_second_writer_waits_for_the_first_to_commit(make_server):
    url = make_server()
    hold_s = 1.0
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        first_has_slot = threading.Event()

        def first() -> None:
            with driver.session() as session:
                tx = session.begin_transaction()
                tx.run("CREATE (:Person {id: 30001, title: 'First'})").consume()
                first_has_slot.set()
                time.sleep(hold_s)
                tx.commit()

        t = threading.Thread(target=first)
        t.start()
        assert first_has_slot.wait(10)
        started = time.monotonic()
        with driver.session() as session:
            tx = session.begin_transaction()
            tx.run("MATCH (n:Person {title: 'First'}) SET n.seen = true").consume()
            waited = time.monotonic() - started
            tx.commit()
        t.join()
        assert waited >= hold_s * 0.6, f"second writer did not queue (waited {waited:.2f}s)"
        with driver.session() as session:
            # It began after the first commit, so it saw that write.
            assert _count(session, "n.title = 'First' AND n.seen = true") == 1


def test_wait_timeout_is_retriable_and_the_holder_is_unharmed(make_server):
    url = make_server("--writer-wait-timeout", "1", "--writer-idle-timeout", "0")
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with driver.session() as holder, driver.session() as waiter:
            tx = holder.begin_transaction()
            tx.run("CREATE (:Person {id: 40001, title: 'Holder'})").consume()
            started = time.monotonic()
            with pytest.raises(neo4j.exceptions.TransientError) as excinfo:
                waiter.begin_transaction().run("CREATE (:Person {id: 40002})").consume()
            assert time.monotonic() - started >= 0.9
            assert excinfo.value.code == WAIT_TIMEOUT_CODE
            assert excinfo.value.is_retryable() is True
            tx.commit()
        with driver.session() as session:
            assert _count(session, "n.title = 'Holder'") == 1
            assert _count(session, "n.id = 40002") == 0


def test_managed_transaction_rides_out_a_wait_timeout(make_server, caplog):
    """The retriable class is what lets `execute_write` recover by itself."""
    url = make_server("--writer-wait-timeout", "1", "--writer-idle-timeout", "0")
    with neo4j.GraphDatabase.driver(url, auth=AUTH, max_transaction_retry_time=30.0, initial_retry_delay=0.1) as driver:
        release = threading.Event()
        held = threading.Event()

        def holder() -> None:
            with driver.session() as session:
                tx = session.begin_transaction()
                tx.run("CREATE (:Person {id: 50001, title: 'Slow'})").consume()
                held.set()
                release.wait(30)
                tx.commit()

        t = threading.Thread(target=holder)
        t.start()
        assert held.wait(10)
        threading.Timer(2.5, release.set).start()  # > one wait timeout: forces >= 1 retry

        def unit(tx):
            tx.run("CREATE (:Person {id: 50002, title: 'Patient'})").consume()

        with caplog.at_level("WARNING", logger="neo4j"):
            with driver.session() as session:
                session.execute_write(unit)
        t.join()
        # A BEGIN that fails never reaches the unit of work, so the driver's own
        # "will be retried" warning is the evidence of the retry.
        retried = [
            r for r in caplog.records if "will be retried" in r.getMessage() and WAIT_TIMEOUT_CODE in r.getMessage()
        ]
        assert retried, "the first attempt should have timed out waiting and been retried"
        with driver.session() as session:
            assert _count(session, "n.title = 'Slow' OR n.title = 'Patient'") == 2


def test_an_idle_holder_is_rolled_back_when_a_writer_waits(make_server):
    url = make_server("--writer-idle-timeout", "1", "--writer-wait-timeout", "30")
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with driver.session() as stalled:
            tx = stalled.begin_transaction()
            tx.run("CREATE (:Person {id: 60001, title: 'Stalled'})").consume()
            started = time.monotonic()
            with driver.session() as other:
                tx2 = other.begin_transaction()
                tx2.run("CREATE (:Person {id: 60002, title: 'Next'})").consume()
                tx2.commit()
            assert 0.8 <= time.monotonic() - started < 15
            with pytest.raises(neo4j.exceptions.ClientError) as excinfo:
                tx.run("CREATE (:Person {id: 60003})").consume()
            assert excinfo.value.code == "Neo.ClientError.Transaction.TransactionTimedOut"
        with driver.session() as session:
            assert _count(session, "n.title = 'Stalled'") == 0
            assert _count(session, "n.title = 'Next'") == 1


def test_a_dropped_connection_frees_the_slot(make_server):
    url = make_server("--writer-wait-timeout", "30", "--writer-idle-timeout", "0")
    doomed = neo4j.GraphDatabase.driver(url, auth=AUTH)
    session = doomed.session()
    tx = session.begin_transaction()
    tx.run("CREATE (:Person {id: 70001, title: 'Doomed'})").consume()
    doomed.close()  # closes the socket with the transaction still open

    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        started = time.monotonic()
        with driver.session() as s:
            tx = s.begin_transaction()
            tx.run("CREATE (:Person {id: 70002, title: 'Survivor'})").consume()
            tx.commit()
        assert time.monotonic() - started < 10, "the slot stayed held after the connection dropped"
        with driver.session() as s:
            assert _count(s, "n.title = 'Doomed'") == 0
            assert _count(s, "n.title = 'Survivor'") == 1


def test_a_read_access_transaction_cannot_write(make_server):
    url = make_server()
    with neo4j.GraphDatabase.driver(url, auth=AUTH) as driver:
        with driver.session() as session:
            with pytest.raises(neo4j.exceptions.ClientError) as excinfo:
                session.execute_read(lambda tx: tx.run("CREATE (:Person {id: 80001})").consume())
            assert excinfo.value.code == "Neo.ClientError.Statement.AccessMode"
        with driver.session() as session:
            assert _count(session, "n.id = 80001") == 0
