"""Schema statements over the Bolt wire, through the neo4j Python driver.

Neo4j runs `CREATE INDEX` / `DROP INDEX` / constraint DDL in auto-commit
transactions (`session.run`), which is where a migration script sends them, and
`CALL db.checkpoint()` is auto-commit only. These tests pin the same contract
on `kglite-bolt-server`:

- schema DDL publishes from `session.run`, reports query type `s`, and a
  refused form publishes nothing;
- data writes stay refused in auto-commit, with the explicit-transaction remedy;
- schema DDL also runs inside an explicit write transaction and commits with
  it. This is a deliberate difference from Neo4j, which refuses a transaction
  that mixes schema and data writes;
- `db.checkpoint()` still works in auto-commit and is refused inside a
  transaction.
"""

import pytest

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt]

AUTH = ("neo4j", "password")


def _index_names(session) -> set[str]:
    return {row["name"] for row in session.run("SHOW INDEXES")}


def test_create_and_drop_index_run_in_auto_commit(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            summary = session.run("CREATE INDEX FOR (n:Person) ON (n.city)").consume()
            assert summary.query_type == "s"
            assert "Person.city" in _index_names(session)

            # The index is live, not just listed: a point lookup answers.
            rows = session.run("MATCH (n:Person {city: $c}) RETURN n.id AS id", c="Oslo").data()
            assert isinstance(rows, list)

            # Re-running without IF NOT EXISTS is a client error, not a crash.
            session.run("CREATE INDEX IF NOT EXISTS FOR (n:Person) ON (n.city)").consume()

            session.run("DROP INDEX `Person.city`").consume()
            assert "Person.city" not in _index_names(session)


def test_a_refused_schema_statement_leaves_the_graph_unchanged(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            before = _index_names(session)
            with pytest.raises(neo4j.exceptions.ClientError):
                session.run("CREATE FULLTEXT INDEX ft FOR (n:Person) ON EACH [n.title]").consume()
            assert _index_names(session) == before


def test_constraints_run_in_auto_commit(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            session.run(
                "CREATE CONSTRAINT person_title_unique FOR (n:Person) REQUIRE n.title IS UNIQUE"
            ).consume()
            names = {row["name"] for row in session.run("SHOW CONSTRAINTS")}
            assert "person_title_unique" in names
            session.run("DROP CONSTRAINT person_title_unique").consume()
            names = {row["name"] for row in session.run("SHOW CONSTRAINTS")}
            assert "person_title_unique" not in names


def test_data_writes_stay_refused_in_auto_commit(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            with pytest.raises(neo4j.exceptions.ClientError) as excinfo:
                session.run("CREATE (:Person {id: 9001, title: 'Nope'})").consume()
            assert "explicit transaction" in str(excinfo.value)
            # A node variable spelled like a DDL keyword is still a data write.
            with pytest.raises(neo4j.exceptions.ClientError):
                session.run("CREATE (index:Person {id: 9002, title: 'Nope'})").consume()
            assert session.run("MATCH (n:Person) RETURN count(n) AS c").single()["c"] == 4


def test_schema_ddl_also_runs_in_an_explicit_write_transaction(bolt_server):
    """Kept permissive on purpose: it commits atomically with the data."""
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:

            def work(tx):
                tx.run("CREATE (:Person {id: 9100, title: 'WithIndex', city: 'Kristiansund'})").consume()
                tx.run("CREATE INDEX FOR (n:Person) ON (n.city)").consume()

            session.execute_write(work)
            assert "Person.city" in _index_names(session)
            row = session.run("MATCH (n:Person {city: 'Kristiansund'}) RETURN n.id AS id").single()
            assert row["id"] == 9100


def test_a_rolled_back_transaction_discards_its_schema_change(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            tx = session.begin_transaction()
            tx.run("CREATE INDEX FOR (n:Person) ON (n.city)").consume()
            tx.rollback()
            assert "Person.city" not in _index_names(session)


def test_checkpoint_is_auto_commit_only(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            record = session.run("CALL db.checkpoint()").single()
            assert record is not None
            tx = session.begin_transaction()
            with pytest.raises(neo4j.exceptions.Neo4jError) as excinfo:
                tx.run("CALL db.checkpoint()").consume()
            assert "explicit transaction" in str(excinfo.value)
            tx.rollback()
