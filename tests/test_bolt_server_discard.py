"""DISCARD keeps the result summary and honours `n`.

`boltr` 0.2.0 answers DISCARD with an empty SUCCESS, ignores `n`, and drops
the summary the driver reads counters, bookmarks and timings from, so
`result.consume().counters.nodes_created` read 0 after a CREATE. The
connection adapters in `crates/kglite-bolt-server/src` turn an incoming
DISCARD into a PULL and drop the RECORDs that answer it; these tests pin that
with the official driver and with a raw client that sees every byte.
"""

import pytest

neo4j = pytest.importorskip("neo4j")

from tests.bolt_raw import (  # noqa: E402
    SIG_DISCARD,
    SIG_FAILURE,
    SIG_IGNORED,
    SIG_PULL,
    SIG_RECORD,
    SIG_RESET,
    SIG_RUN,
    SIG_SUCCESS,
    RawBolt,
)
from tests.conftest import (  # noqa: E402
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt]


@pytest.fixture
def url(tmp_path):
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    graph = tmp_path / "discard.kgl"
    _build_bolt_fixture_graph(graph)
    proc, bolt_url = _spawn_bolt_server(graph)
    try:
        yield bolt_url
    finally:
        _teardown_bolt_server(proc)


@pytest.fixture
def raw(url):
    host, port = url.removeprefix("bolt://").rsplit(":", 1)
    with RawBolt(host, int(port)) as conn:
        conn.hello()
        conn.logon("u", "p")
        yield conn


def _driver(url, **kw):
    return neo4j.GraphDatabase.driver(url, auth=("neo4j", "password"), **kw)


def _until_summary(conn: RawBolt) -> list[tuple[int, bytes]]:
    """Every message up to and including the next non-RECORD."""
    seen = []
    while True:
        message = conn.recv()
        seen.append(message)
        if message[0] != SIG_RECORD:
            return seen


# --- official driver ------------------------------------------------------------


def test_consume_after_a_write_in_a_transaction_reports_counters(url):
    with _driver(url) as driver, driver.session() as s:

        def work(tx):
            return tx.run("CREATE (:DiscardX {v: 1})").consume().counters.nodes_created

        assert s.execute_write(work) == 1


def test_consume_in_an_explicit_transaction_reports_counters_and_commits(url):
    with _driver(url) as driver, driver.session() as s:
        tx = s.begin_transaction()
        summary = tx.run("CREATE (:DiscardX {v: 1}), (:DiscardX {v: 2})").consume()
        tx.commit()
        assert summary.counters.nodes_created == 2
        assert s.run("MATCH (n:DiscardX) RETURN count(n) AS c").single()["c"] == 2


def test_consume_of_an_auto_commit_read_returns_the_summary(url):
    query = "UNWIND range(1, 50) AS x RETURN x"
    with _driver(url) as driver, driver.session() as s:
        summary = s.run(query).consume()
    assert summary.query == query
    assert summary.result_consumed_after is not None and summary.result_consumed_after >= 0


def test_consume_after_a_partial_read_keeps_the_summary_and_the_connection(url):
    with _driver(url, fetch_size=100) as driver, driver.session() as s:

        def work(tx):
            result = tx.run("UNWIND range(1, 500) AS x CREATE (:DiscardX {v: x}) RETURN x")
            assert next(iter(result))["x"] == 1
            return result.consume()

        summary = s.execute_write(work)
        assert summary.counters.nodes_created == 500
        assert summary.result_consumed_after is not None
        assert s.run("MATCH (n:DiscardX) RETURN count(n) AS c").single()["c"] == 500


# --- raw client -----------------------------------------------------------------


def test_discard_sends_no_records_and_one_success(raw):
    assert raw.request(SIG_RUN, "UNWIND range(1, 1000) AS x RETURN x", {}, {}) == SIG_SUCCESS
    raw.send(SIG_DISCARD, {"n": -1})
    messages = _until_summary(raw)
    assert [sig for sig, _ in messages] == [SIG_SUCCESS], "no RECORD may reach the client"


def test_discard_smaller_than_the_result_reports_has_more_and_the_next_pull_keeps_its_rows(raw):
    assert raw.request(SIG_RUN, "UNWIND range(1, 10) AS x RETURN x", {}, {}) == SIG_SUCCESS
    raw.send(SIG_DISCARD, {"n": 3})
    first = _until_summary(raw)
    assert [sig for sig, _ in first] == [SIG_SUCCESS]
    assert b"has_more" in first[0][1], "a partial DISCARD must say more is left"
    raw.send(SIG_PULL, {"n": 2})
    pulled = _until_summary(raw)
    assert [sig for sig, _ in pulled] == [SIG_RECORD, SIG_RECORD, SIG_SUCCESS], (
        "a PULL after a DISCARD keeps its records"
    )
    raw.send(SIG_DISCARD, {"n": -1})
    last = _until_summary(raw)
    assert [sig for sig, _ in last] == [SIG_SUCCESS]


def test_two_discards_in_a_row_drain_a_result(raw):
    assert raw.request(SIG_RUN, "UNWIND range(1, 10) AS x RETURN x", {}, {}) == SIG_SUCCESS
    for n in (4, -1):
        raw.send(SIG_DISCARD, {"n": n})
        assert [sig for sig, _ in _until_summary(raw)] == [SIG_SUCCESS]
    assert raw.request(SIG_RUN, "RETURN 1 AS one", {}, {}) == SIG_SUCCESS


def test_pipelined_discard_and_pull_answer_in_order(raw):
    assert raw.request(SIG_RUN, "UNWIND range(1, 10) AS x RETURN x", {}, {}) == SIG_SUCCESS
    raw.send(SIG_DISCARD, {"n": 2})
    raw.send(SIG_PULL, {"n": 3})
    raw.send(SIG_DISCARD, {"n": -1})
    sigs = []
    while sigs.count(SIG_SUCCESS) < 3:
        sigs.append(raw.recv()[0])
    assert sigs == [SIG_SUCCESS, SIG_RECORD, SIG_RECORD, SIG_RECORD, SIG_SUCCESS, SIG_SUCCESS]


def test_a_failure_during_a_discarded_query_does_not_disturb_the_next_exchange(raw):
    raw.send(SIG_RUN, "UNWIND range(1, 5) AS x RETURN x / 0", {}, {})
    raw.send(SIG_DISCARD, {"n": -1})
    first = raw.recv()[0]
    assert first == SIG_FAILURE
    assert raw.recv()[0] == SIG_IGNORED
    assert raw.request(SIG_RESET) == SIG_SUCCESS
    assert raw.request(SIG_RUN, "UNWIND range(1, 4) AS x RETURN x", {}, {}) == SIG_SUCCESS
    raw.send(SIG_PULL, {"n": -1})
    sigs = [sig for sig, _ in _until_summary(raw)]
    assert sigs == [SIG_RECORD] * 4 + [SIG_SUCCESS], "records after a discarded failure must not be dropped"
