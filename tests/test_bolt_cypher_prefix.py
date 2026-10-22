"""The `CYPHER 5` / `CYPHER 25` statement prefix over Bolt: looked through for
the server's own verbs, and an unsupported version reaches the client as an
error."""

from __future__ import annotations

import pytest

from tests.conftest import (
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt]

neo4j = pytest.importorskip("neo4j")
from neo4j import GraphDatabase  # noqa: E402


def _serve(tmp_path, name, extra_args):
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    fixture_path = tmp_path / f"maps_{name}.kgl"
    _build_bolt_fixture_graph(fixture_path)
    proc, url = _spawn_bolt_server(fixture_path, extra_args=extra_args)
    try:
        yield url
    finally:
        _teardown_bolt_server(proc)


@pytest.fixture
def compat_url(tmp_path):
    yield from _serve(tmp_path, "compat", ["--neo4j-compat"])


def _run(url, query, **params):
    with GraphDatabase.driver(url, auth=None) as driver:
        with driver.session() as session:
            return list(session.run(query, **params))


def test_cypher_prefix_is_looked_through_for_server_verbs(compat_url):
    rec = _run(compat_url, "CYPHER 25 CALL dbms.components() YIELD name, versions, edition")[0]
    assert rec["name"] == "Neo4j Kernel"
    rec = _run(compat_url, "CYPHER 5 CALL dbms.components() YIELD name RETURN name AS n")[0]
    assert rec["n"] == "Neo4j Kernel"
    assert _run(compat_url, "CYPHER 25 SHOW DATABASES")[0]["name"] == "neo4j"
    assert _run(compat_url, "CYPHER 5 RETURN 1 AS one")[0]["one"] == 1


def test_cypher_prefix_errors_reach_the_client(compat_url):
    with pytest.raises(neo4j.exceptions.ClientError) as info:
        _run(compat_url, "CYPHER 99 RETURN 1")
    assert "Unsupported Cypher version" in str(info.value)


def test_graphbridge_min_max_integer_guard_over_bolt(compat_url):
    _run(compat_url, "CREATE (:T {p: 1}), (:T {p: 3.0}), (:T {p: 'x'})")
    rows = _run(
        compat_url,
        "MATCH (n:T) WITH n.p AS p RETURN count(CASE WHEN p IS :: INTEGER NOT NULL THEN null ELSE 1 END) AS bad",
    )
    assert rows[0]["bad"] == 2
