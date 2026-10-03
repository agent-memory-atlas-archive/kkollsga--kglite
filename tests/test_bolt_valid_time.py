"""Valid-time statements over Bolt: the echo in the SUCCESS metadata's
``kglite.temporal`` key, and the streaming aggregate shapes answering as the
engine's eager path does."""

from __future__ import annotations

import pytest

import kglite
from tests.valid_time_network import AT, NETWORK, STREAMING_SHAPES

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt]


@pytest.fixture
def served(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=("neo4j", "password")) as driver:
        with driver.session() as session:
            with session.begin_transaction() as tx:
                for statement in NETWORK:
                    tx.run(statement).consume()
                tx.commit()
            yield session


def _rows(records, ordered):
    rows = [tuple(sorted(dict(r).items())) for r in records]
    return rows if ordered else sorted(rows)


def test_the_summary_carries_the_valid_time_echo(served):
    result = served.run(AT + "MATCH (s:Stop) RETURN s.id AS id ORDER BY id")
    assert [r["id"] for r in result] == [1, 3, 4, 5]
    echo = result.consume().metadata["kglite.temporal"]
    assert echo["axis"] == "VALID_TIME"
    assert echo["instant"] == "2008-01-01"
    assert echo["targets"] == ["(:Stop)"]
    # Stop 2 has closed; the statement names no relationship, so no edge is judged.
    assert echo["hidden"] == {"(:Stop)": 1}
    assert echo["endpoint_invalid"] == 0
    assert echo["route"] == "guarded"
    assert echo["retrieval"] is None
    assert echo["slice"] is False
    assert isinstance(echo["session_version"], int)

    # An unprefixed statement on a declaring graph runs as of today and says so.
    plain = served.run("MATCH (s:Stop) RETURN s.id AS id").consume()
    assert plain.metadata["kglite.temporal"]["source"] == "default"

    # The hop judges the relationship target: the two 2000-2005 links are hidden
    # by their own bounds, and the two links touching the closed stop 2 by it.
    hop = served.run(AT + "MATCH (a:Stop)-[:LINK]->(b:Stop) RETURN count(*) AS n")
    echo = hop.consume().metadata["kglite.temporal"]
    assert echo["hidden"] == {"(:Stop)": 1, "[:LINK]": 2}
    assert echo["endpoint_invalid"] == 2


@pytest.mark.parametrize("shape", STREAMING_SHAPES)
def test_the_streaming_shapes_answer_as_the_eager_path(served, shape):
    reference = kglite.KnowledgeGraph()
    for statement in NETWORK:
        reference.cypher(statement).to_list()
    ordered = "ORDER BY" in shape
    for query in (shape, AT + shape):
        expected = _rows(reference.cypher(query, streaming=False).to_list(), ordered)
        assert _rows(served.run(query), ordered) == expected, query


@pytest.mark.parametrize(
    "statement", [AT + "EXPLAIN MATCH (s:Stop) RETURN s.id", "EXPLAIN " + AT + "MATCH (s:Stop) RETURN s.id"]
)
def test_explain_under_a_context_answers_as_a_plan_not_as_records(served, statement):
    """The prefix may sit on either side of EXPLAIN. Bolt used to spot EXPLAIN
    by the first keyword, so the prefix-first spelling forwarded the plan's
    step rows as records with no ``plan`` in the SUCCESS metadata."""
    result = served.run(statement)
    assert list(result) == []
    summary = result.consume()
    assert summary.plan is not None
    assert summary.plan["args"]["runtime"] == "kglite"
    assert summary.metadata["kglite.temporal"]["route"] == "guarded"


def test_a_graph_without_declarations_carries_no_echo(bolt_server):
    """No declaration, no valid time to report: neither the default nor an
    ALL prefix puts a ``kglite.temporal`` key on the summary."""
    with neo4j.GraphDatabase.driver(bolt_server, auth=("neo4j", "password")) as driver:
        with driver.session() as session:
            with session.begin_transaction() as tx:
                tx.run("CREATE (:Stop {id: 1})").consume()
                tx.commit()
            for query in ("MATCH (s:Stop) RETURN s.id AS id", "FOR VALID_TIME ALL MATCH (s:Stop) RETURN s.id AS id"):
                result = session.run(query)
                assert [r["id"] for r in result] == [1]
                assert "kglite.temporal" not in result.consume().metadata


def test_the_all_prefix_reads_every_version_over_bolt(served):
    """`FOR VALID_TIME ALL` is text the Bolt server forwards as it stands: no
    new Bolt surface, and the echo names the source."""
    default = served.run("MATCH (s:Stop) RETURN s.id AS id ORDER BY id")
    assert [r["id"] for r in default] == [1, 3, 4, 5]
    assert default.consume().metadata["kglite.temporal"]["source"] == "default"
    result = served.run("FOR VALID_TIME ALL MATCH (s:Stop) RETURN s.id AS id ORDER BY id")
    assert [r["id"] for r in result] == [1, 2, 3, 4, 5]
    echo = result.consume().metadata["kglite.temporal"]
    assert (echo["source"], echo["instant"]) == ("all", "all")


def test_the_valid_time_default_flag_governs_unprefixed_statements(tmp_path):
    from tests.conftest import (
        _bolt_binary_available,
        _build_bolt_fixture_graph,
        _spawn_bolt_server,
        _teardown_bolt_server,
    )

    if not _bolt_binary_available():
        pytest.skip("kglite-bolt-server binary is not built")
    for flag, expected, instant in [
        ("all", [1, 2, 3, 4, 5], "all"),
        ("2003-01-01", [1, 2, 3, 4, 5], "2003-01-01"),
        ("2008-01-01", [1, 3, 4, 5], "2008-01-01"),
    ]:
        path = tmp_path / f"fixture-{flag}.kgl"
        _build_bolt_fixture_graph(path)
        proc, url = _spawn_bolt_server(path, extra_args=["--valid-time-default", flag])
        try:
            with neo4j.GraphDatabase.driver(url, auth=("neo4j", "password")) as driver:
                with driver.session() as session:
                    with session.begin_transaction() as tx:
                        for statement in NETWORK:
                            tx.run(statement).consume()
                        tx.commit()
                    result = session.run("MATCH (s:Stop) RETURN s.id AS id ORDER BY id")
                    assert [r["id"] for r in result] == expected, flag
                    echo = result.consume().metadata["kglite.temporal"]
                    assert (echo["source"], echo["instant"]) == ("default", instant), flag
        finally:
            _teardown_bolt_server(proc)
