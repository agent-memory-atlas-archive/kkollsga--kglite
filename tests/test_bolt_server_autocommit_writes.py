"""Auto-commit data writes over real Bolt, through the official `neo4j` driver.

`session.run("CREATE ...")` is a transaction of its own: it commits before the
result is returned, reports its counters, and is visible to the next session.
Drivers never retry `session.run`, so the server queues contending writers
instead of surfacing a conflict.
"""

from concurrent.futures import ThreadPoolExecutor

import pytest

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt]

AUTH = ("neo4j", "password")


def _count(session, where: str) -> int:
    return session.run(f"MATCH (n:Person) WHERE {where} RETURN count(n) AS c").single()["c"]


def test_an_auto_commit_write_is_visible_to_the_next_session(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as writer:
            writer.run("CREATE (:Person {id: 7001, title: 'AutoA'})").consume()
        with driver.session() as reader:
            assert _count(reader, "n.title = 'AutoA'") == 1


def test_the_summary_reports_counters_and_query_type(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            summary = session.run("CREATE (:Person {id: 7002, title: 'AutoB'})").consume()
            assert summary.counters.nodes_created == 1
            assert summary.query_type == "w"


def test_a_write_with_return_streams_its_rows(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            result = session.run("CREATE (n:Person {id: 7003, title: 'AutoC'}) RETURN n.id AS id, n.title AS t")
            rows = list(result)
            assert [(r["id"], r["t"]) for r in rows] == [(7003, "AutoC")]
            summary = result.consume()
            assert summary.counters.nodes_created == 1
            assert summary.query_type == "rw"
            assert _count(session, "n.title = 'AutoC'") == 1


def test_a_failing_write_applies_nothing(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            with pytest.raises(neo4j.exceptions.ClientError):
                session.run("CREATE (:Person {id: 7004, title: 'Half'}) WITH 1 AS x RETURN x / 0").consume()
            assert _count(session, "n.title = 'Half'") == 0


def test_a_write_in_a_read_session_is_an_access_mode_error(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session(default_access_mode=neo4j.READ_ACCESS) as session:
            with pytest.raises(neo4j.exceptions.ClientError) as excinfo:
                session.run("CREATE (:Person {id: 7005, title: 'ReadMode'})").consume()
            assert excinfo.value.code == "Neo.ClientError.Statement.AccessMode"
            assert _count(session, "n.title = 'ReadMode'") == 0
            # Reads in the same session are unaffected.
            assert _count(session, "true") == 4


def test_execute_write_still_works(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            session.execute_write(lambda tx: tx.run("CREATE (:Person {id: 7006, title: 'Managed'})").consume())
            assert _count(session, "n.title = 'Managed'") == 1


def test_explain_of_a_write_changes_nothing(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            session.run("EXPLAIN CREATE (:Person {id: 7007, title: 'Planned'})").consume()
            assert _count(session, "n.title = 'Planned'") == 0


def test_concurrent_auto_commit_writers_all_apply_without_errors(bolt_server):
    writers, rounds = 4, 10

    def work(w: int) -> int:
        done = 0
        with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
            with driver.session() as session:
                for r in range(rounds):
                    session.run(f"CREATE (:Person {{id: {8000 + w * 100 + r}, title: 'Conc{w}'}})").consume()
                    done += 1
        return done

    with ThreadPoolExecutor(max_workers=writers) as pool:
        assert sum(pool.map(work, range(writers))) == writers * rounds
    with neo4j.GraphDatabase.driver(bolt_server, auth=AUTH) as driver:
        with driver.session() as session:
            assert _count(session, "n.title STARTS WITH 'Conc'") == writers * rounds
