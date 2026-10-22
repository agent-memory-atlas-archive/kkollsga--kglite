"""`--neo4j-compat` serves Neo4j-shaped property maps, over the wire.

`keys(n)`, `properties(n)` and `n {.*}` carry stored properties only under the
compatibility identity (kglite's virtual `title` and `type` are left out) and
keep kglite's shape under the honest one.
"""

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


@pytest.fixture
def honest_url(tmp_path):
    yield from _serve(tmp_path, "honest", [])


def _run(url, query, **params):
    with GraphDatabase.driver(url, auth=None) as driver:
        with driver.session() as session:
            return list(session.run(query, **params))


JOURNAL = "CREATE (:__GraphBridgeMigrationControl {id: 'database', formatVersion: 1, generation: 0, lock: 0})"
READ_CONTROL = "MATCH (c:__GraphBridgeMigrationControl) RETURN properties(c) AS record LIMIT $limit"


def test_the_graphbridge_journal_record_is_its_stored_properties(compat_url):
    _run(compat_url, JOURNAL)
    record = _run(compat_url, READ_CONTROL, limit=2)[0]["record"]
    assert record == {"id": "database", "formatVersion": 1, "generation": 0, "lock": 0}


def test_the_honest_identity_keeps_the_structural_keys(honest_url):
    _run(honest_url, JOURNAL)
    record = _run(honest_url, READ_CONTROL, limit=2)[0]["record"]
    assert {"id", "title", "type"} <= set(record)
    assert record["type"] == "__GraphBridgeMigrationControl"


def test_keys_and_projection_follow_the_identity(compat_url, honest_url):
    for url, expected in (
        (compat_url, ["formatVersion", "generation", "id", "lock"]),
        (honest_url, ["formatVersion", "generation", "id", "lock", "title", "type"]),
    ):
        _run(url, JOURNAL)
        assert _run(url, "MATCH (c:__GraphBridgeMigrationControl) RETURN keys(c) AS k")[0]["k"] == expected
        projected = _run(url, "MATCH (c:__GraphBridgeMigrationControl) RETURN c {.*} AS m")[0]["m"]
        assert sorted(projected) == expected


def test_user_properties_named_title_and_type_stay(compat_url):
    _run(compat_url, "CREATE (:Doc {id: 1, title: 'Hello', type: 'memo'})")
    record = _run(compat_url, "MATCH (d:Doc) RETURN properties(d) AS p")[0]["p"]
    assert record == {"id": 1, "title": "Hello", "type": "memo"}


def test_a_returned_node_is_unchanged(compat_url):
    _run(compat_url, JOURNAL)
    node = _run(compat_url, "MATCH (c:__GraphBridgeMigrationControl) RETURN c")[0]["c"]
    assert {"id", "title", "type", "formatVersion"} <= set(node.keys())
