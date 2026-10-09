"""Server-side query limits of kglite-bolt-server: `--query-timeout`,
`--max-work-units`, `--max-rows`, and the client's Bolt `tx_timeout`."""

import os
import time

import pandas as pd
import pytest

import kglite

neo4j = pytest.importorskip("neo4j")

from tests.conftest import (  # noqa: E402
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt, pytest.mark.skipif(os.name != "posix", reason="POSIX process control")]

N = 500
# N^3 candidate triples: far beyond any millisecond budget, but each step
# checks the deadline, so a timeout stops it promptly.
SLOW = "MATCH (a:P), (b:P), (c:P) WHERE a.id + b.id + c.id = -1 RETURN count(*) AS c"


@pytest.fixture(autouse=True)
def _require_binary():
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)


@pytest.fixture
def graph_path(tmp_path):
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": list(range(N)), "title": [f"p{i}" for i in range(N)]}), "P", "id", "title")
    path = tmp_path / "limits.kgl"
    g.save(str(path))
    return path


def _serve(graph_path, *flags):
    proc, url = _spawn_bolt_server(graph_path, extra_args=list(flags))
    driver = neo4j.GraphDatabase.driver(url, auth=("neo4j", "password"))
    return proc, driver


@pytest.fixture
def served(graph_path):
    started = []

    def start(*flags):
        proc, driver = _serve(graph_path, *flags)
        started.append((proc, driver))
        return driver

    yield start
    for proc, driver in started:
        driver.close()
        _teardown_bolt_server(proc)


def _timed_out(exc) -> bool:
    return "TransactionTimedOut" in str(getattr(exc, "code", "")) or "imed out" in str(exc)


def test_query_timeout_stops_a_slow_auto_commit_query(served):
    driver = served("--query-timeout", "300")
    with driver.session() as s:
        started = time.monotonic()
        with pytest.raises(neo4j.exceptions.Neo4jError) as err:
            s.run(SLOW).consume()
        assert _timed_out(err.value), err.value
        assert time.monotonic() - started < 20
        assert s.run("RETURN 1 AS x").single()["x"] == 1


def test_query_timeout_applies_inside_an_explicit_transaction(served):
    driver = served("--query-timeout", "300")
    with driver.session() as s:
        tx = s.begin_transaction()
        with pytest.raises(neo4j.exceptions.Neo4jError) as err:
            tx.run(SLOW).consume()
        assert _timed_out(err.value), err.value


def test_no_flag_leaves_cheap_queries_untouched(served):
    driver = served()
    with driver.session() as s:
        assert s.run("MATCH (n:P) RETURN count(n) AS c").single()["c"] == N


def test_client_tx_timeout_is_honoured_without_a_server_limit(served):
    driver = served()
    with driver.session() as s:
        with pytest.raises(neo4j.exceptions.Neo4jError) as err:
            s.run(neo4j.Query(SLOW, timeout=0.3)).consume()
        assert _timed_out(err.value), err.value


def test_client_tx_timeout_is_capped_by_query_timeout(served):
    driver = served("--query-timeout", "300")
    with driver.session() as s:
        started = time.monotonic()
        with pytest.raises(neo4j.exceptions.Neo4jError) as err:
            s.run(neo4j.Query(SLOW, timeout=3600)).consume()
        assert _timed_out(err.value), err.value
        assert time.monotonic() - started < 30


def test_tx_timeout_on_begin_applies_to_its_statements(served):
    driver = served()
    with driver.session() as s:
        tx = s.begin_transaction(timeout=0.3)
        with pytest.raises(neo4j.exceptions.Neo4jError) as err:
            tx.run(SLOW).consume()
        assert _timed_out(err.value), err.value


def test_max_rows_truncates_and_reports_the_total(served):
    driver = served("--max-rows", "5")
    with driver.session() as s:
        result = s.run("MATCH (n:P) RETURN n.id AS id ORDER BY id")
        ids = [r["id"] for r in result]
        summary = result.consume()
    assert ids == [0, 1, 2, 3, 4]
    meta = summary.metadata["kglite.row_limit"]
    assert meta == {"limit": 5, "total_rows": N}


def test_max_rows_does_not_flag_a_result_within_the_cap(served):
    driver = served("--max-rows", "5")
    with driver.session() as s:
        result = s.run("MATCH (n:P) RETURN n.id AS id LIMIT 3")
        assert len(list(result)) == 3
        assert "kglite.row_limit" not in result.consume().metadata


def test_max_work_units_fails_the_query_instead_of_truncating(served):
    driver = served("--max-work-units", "50")
    with driver.session() as s:
        with pytest.raises(neo4j.exceptions.Neo4jError) as err:
            list(s.run("MATCH (n:P) RETURN n.id AS id"))
        assert "max_work_units" in str(err.value)
