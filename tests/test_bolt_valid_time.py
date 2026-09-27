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
    assert echo["route"] == "guarded"
    assert echo["retrieval"] is None
    assert echo["slice"] is False
    assert isinstance(echo["session_version"], int)

    plain = served.run("MATCH (s:Stop) RETURN s.id AS id").consume()
    assert "kglite.temporal" not in plain.metadata


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
