"""Bolt RUN SUCCESS ``fields`` carry an unaliased item as the query wrote it."""

import pytest

neo4j = pytest.importorskip("neo4j")

pytestmark = [pytest.mark.bolt]


def test_unaliased_items_are_named_by_their_source_text(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=("neo4j", "password")) as driver:
        with driver.session() as session:
            result = session.run("RETURN toInteger('3'), size([1,2]), 'a', 1+2")
            assert result.keys() == ["toInteger('3')", "size([1,2])", "'a'", "1+2"]
            record = result.single()
            assert record["toInteger('3')"] == 3
            assert record["1+2"] == 3
            assert record.keys() == ["toInteger('3')", "size([1,2])", "'a'", "1+2"]


def test_aliases_and_ordering_are_unaffected(bolt_server):
    with neo4j.GraphDatabase.driver(bolt_server, auth=("neo4j", "password")) as driver:
        with driver.session() as session:
            result = session.run("MATCH (n:Person) RETURN toUpper(n.title), n.title AS t ORDER BY toUpper(n.title) DESC")
            assert result.keys() == ["toUpper(n.title)", "t"]
            assert [r["t"] for r in result] == ["Dave", "Carol", "Bob", "Alice"]
