"""GraphBridge's statements over a user `id` key, through Bolt `--neo4j-compat`.

The migration journal declares `REQUIRE n.id IS UNIQUE`, checks that a RANGE
index backs it, creates its control row with `MERGE … {id: $id}`, and locks it
with `SET n.id = n.id`. Entity writes are `CREATE (n:L) SET n = $properties`
and `SET n += $changes`.
"""

from __future__ import annotations

import pytest

from tests.conftest import (
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt]

neo4j = pytest.importorskip("neo4j")
from neo4j import GraphDatabase  # noqa: E402

CODE = "Neo.ClientError.Schema.ConstraintValidationFailed"
JOURNAL_KEY = (
    "CREATE CONSTRAINT `gb_migration_control_key` FOR (n:`__GraphBridgeMigrationControl`) REQUIRE n.`id` IS UNIQUE"
)
CONTROL_MERGE = (
    "MERGE (c:__GraphBridgeMigrationControl {id: $id}) "
    "ON CREATE SET c.formatVersion = 1, c.generation = 0, c.lock = 0 RETURN c.id"
)


@pytest.fixture
def url(tmp_path):
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    import kglite

    path = tmp_path / "empty.kgl"
    kglite.KnowledgeGraph().save(str(path))
    proc, address = _spawn_bolt_server(path, extra_args=["--neo4j-compat"])
    try:
        yield address
    finally:
        _teardown_bolt_server(proc)


def _run(session, statement, **params):
    return [record.data() for record in session.run(statement, params)]


def test_the_journal_declares_checks_merges_and_locks_over_id(url):
    with GraphDatabase.driver(url, auth=None) as driver, driver.session() as session:
        session.run(JOURNAL_KEY).consume()
        rows = _run(
            session,
            "SHOW INDEXES YIELD name, type, state, entityType, labelsOrTypes, properties, owningConstraint "
            "RETURN name, type, state, entityType, labelsOrTypes, properties, owningConstraint",
        )
        backing = [r for r in rows if r["owningConstraint"] == "gb_migration_control_key"]
        assert len(backing) == 1
        assert backing[0]["type"] == "RANGE" and backing[0]["state"] == "ONLINE"
        assert backing[0]["labelsOrTypes"] == ["__GraphBridgeMigrationControl"] and backing[0]["properties"] == ["id"]
        constraints = _run(session, "SHOW CONSTRAINTS YIELD name, type, properties")
        assert constraints == [{"name": "gb_migration_control_key", "type": "UNIQUENESS", "properties": ["id"]}]

        assert _run(session, CONTROL_MERGE, id="database") == [{"c.id": "database"}]
        assert _run(session, CONTROL_MERGE, id="database") == [{"c.id": "database"}]
        session.run(
            "MATCH (c:__GraphBridgeMigrationControl {id: $id}) SET c.lock = c.lock + 1", id="database"
        ).consume()
        session.run("MATCH (c:__GraphBridgeMigrationControl {id: $id}) SET c.`id` = c.`id`", id="database").consume()
        record = _run(session, "MATCH (c:__GraphBridgeMigrationControl) RETURN properties(c) AS record")[0]["record"]
        assert (record["id"], record["formatVersion"], record["lock"]) == ("database", 1, 1)


def test_entity_writes_create_update_and_collide_by_id(url):
    with GraphDatabase.driver(url, auth=None) as driver, driver.session() as session:
        session.run("CREATE CONSTRAINT `person_key` FOR (n:`Person`) REQUIRE (n.`id`) IS UNIQUE").consume()
        create = "CREATE (n:`Person`) SET n = $properties"
        session.run(create, properties={"id": "p1", "name": "Ada", "age": 36}).consume()
        session.run("MATCH (n:`Person` {`id`: $key}) SET n += $changes", key="p1", changes={"age": 37}).consume()
        rows = _run(session, "MATCH (n:Person) RETURN n.id AS id, n.name AS name, n.age AS age")
        assert rows == [{"id": "p1", "name": "Ada", "age": 37}]
        with pytest.raises(neo4j.exceptions.ClientError) as raised:
            session.run(create, properties={"id": "p1", "name": "Dup"}).consume()
        assert raised.value.code == CODE, raised.value.code
        with pytest.raises(neo4j.exceptions.ClientError) as raised:
            session.run("CREATE (:Person {id: 'p1'})").consume()
        assert raised.value.code == CODE, raised.value.code
        assert _run(session, "MATCH (n:Person) RETURN count(n) AS c") == [{"c": 1}]
