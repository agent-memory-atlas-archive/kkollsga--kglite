"""`IS :: TYPE`, `valueType()`, the `CYPHER` statement prefix and
`hide_structural_keys`, through the Python wheel, on every storage mode.

Absolute expected values: the differential corpus cannot see a defect the
optimised and unoptimised paths share. The Rust twins live in
`type_predicate_tests.rs`, `cypher_prefix_tests.rs` and
`hidden_structural_keys_tests.rs`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import kglite

from .test_value_variants import STORAGE_MODES, _new_graph


@pytest.fixture(params=STORAGE_MODES)
def graph(request, tmp_path: Path):
    return _new_graph(request.param, tmp_path)


def scalar(g, query, **kwargs):
    rows = list(g.cypher(query, **kwargs))
    assert len(rows) == 1, query
    return next(iter(rows[0].values()))


FAMILIES = {
    "BOOLEAN": "true",
    "STRING": "'a'",
    "INTEGER": "1",
    "FLOAT": "1.5",
    "DATE": "date('2026-01-02')",
    "LOCAL TIME": "localtime('10:00:00')",
    "ZONED TIME": "time('10:00:00+01:00')",
    "LOCAL DATETIME": "localdatetime('2026-01-02T10:00:00')",
    "ZONED DATETIME": "datetime('2026-01-02T10:00:00+01:00')",
    "DURATION": "duration('P1D')",
    "POINT": "point({latitude: 1.0, longitude: 2.0})",
    "MAP": "{a: 1}",
    "LIST<ANY>": "[1, 2]",
}


def test_each_family_matches_only_its_own_type(graph):
    for own, expr in FAMILIES.items():
        for ty in FAMILIES:
            assert scalar(graph, f"RETURN {expr} IS :: {ty}") is (ty == own), f"{expr} IS :: {ty}"
            assert scalar(graph, f"RETURN {expr} IS NOT :: {ty}") is (ty != own)


def test_null_rule_and_not_null(graph):
    assert scalar(graph, "RETURN null IS :: INTEGER") is True
    assert scalar(graph, "RETURN null IS :: INTEGER NOT NULL") is False
    assert scalar(graph, "RETURN 1 IS :: INTEGER NOT NULL") is True
    assert scalar(graph, "RETURN 3.0 IS :: INTEGER") is False
    assert scalar(graph, "RETURN 3 IS :: FLOAT") is False


def test_unions_lists_and_synonyms(graph):
    assert scalar(graph, "RETURN 2.5 IS :: INTEGER | FLOAT") is True
    assert scalar(graph, "RETURN true IS :: INTEGER | FLOAT") is False
    assert scalar(graph, "RETURN [1, 'a'] IS :: LIST<INTEGER | STRING>") is True
    assert scalar(graph, "RETURN [1, null] IS :: LIST<INTEGER NOT NULL>") is False
    assert scalar(graph, "RETURN [] IS :: LIST<INTEGER NOT NULL>") is True
    assert scalar(graph, "RETURN 'x' IS TYPED STRING") is True
    assert scalar(graph, "RETURN 1 IS :: SIGNED INTEGER") is True


def test_the_graphbridge_integer_guard(graph):
    graph.cypher("CREATE (:T {p: 1}), (:T {p: 3.0}), (:T {p: 'x'}), (:T {p: 2147483648}), (:T {q: 1})")
    query = "MATCH (n:T) WITH n.p AS p RETURN count(CASE WHEN {valid} THEN null ELSE 1 END) AS bad"
    assert scalar(graph, query.format(valid="p IS :: INTEGER NOT NULL")) == 3
    bounded = "p IS :: INTEGER NOT NULL AND p >= -2147483648 AND p <= 2147483647"
    assert scalar(graph, query.format(valid=bounded)) == 4
    assert scalar(graph, query.format(valid=f"p IS NULL OR ({bounded})")) == 3


def test_where_position_and_column_name(graph):
    graph.cypher("CREATE (:T {id: 1, p: 1}), (:T {id: 2, p: 'x'})")
    rows = list(graph.cypher("MATCH (n:T) WHERE n.p IS :: INTEGER RETURN n.id AS id"))
    assert [r["id"] for r in rows] == [1]
    view = graph.cypher("MATCH (n:T) RETURN n.p IS NOT :: INTEGER NOT NULL ORDER BY n.id")
    assert view.columns == ["n.p IS NOT :: INTEGER NOT NULL"]


def test_unknown_types_are_refused_with_the_list(graph):
    with pytest.raises(kglite.CypherSyntaxError) as info:
        graph.cypher("RETURN 1 IS :: BLOB")
    assert "BLOB" in str(info.value) and "INTEGER" in str(info.value)


def test_value_type_strings(graph):
    cases = {
        "1": "INTEGER NOT NULL",
        "null": "NULL",
        "'a'": "STRING NOT NULL",
        "[]": "LIST<NOTHING> NOT NULL",
        "[1, null]": "LIST<INTEGER> NOT NULL",
        "[1, 2]": "LIST<INTEGER NOT NULL> NOT NULL",
        "{a: 1}": "MAP NOT NULL",
        "date('2026-01-02')": "DATE NOT NULL",
    }
    for expr, want in cases.items():
        assert scalar(graph, f"RETURN valueType({expr})") == want, expr


def test_value_type_on_entities(graph):
    graph.cypher("CREATE (:A {id: 1})-[:R]->(:B {id: 2})")
    assert scalar(graph, "MATCH (a:A)-[r]->() RETURN valueType(a) + ' ' + valueType(r)") == (
        "NODE NOT NULL RELATIONSHIP NOT NULL"
    )


# ── CYPHER prefix ──────────────────────────────────────────────────────────


def test_cypher_prefix_runs_any_statement(graph):
    assert scalar(graph, "CYPHER 5 RETURN 1 AS one") == 1
    assert scalar(graph, "CYPHER 25 runtime=slotted RETURN 2 AS two") == 2
    graph.cypher("CYPHER 25 CREATE (:P {id: 1, v: 5})")
    assert scalar(graph, "CYPHER 5 MATCH (n:P) RETURN n.v") == 5
    plan = graph.cypher("CYPHER 5 EXPLAIN MATCH (n:P) RETURN n")
    assert len(list(plan)) >= 1
    plan = graph.cypher("EXPLAIN CYPHER 25 MATCH (n:P) RETURN n")
    assert len(list(plan)) >= 1


def test_cypher_prefix_refusals(graph):
    with pytest.raises(kglite.CypherSyntaxError, match="Unsupported Cypher version"):
        graph.cypher("CYPHER 99 RETURN 1")
    with pytest.raises(kglite.CypherSyntaxError, match="Unknown CYPHER option 'turbo'"):
        graph.cypher("CYPHER turbo=on RETURN 1")
    with pytest.raises(kglite.CypherSyntaxError, match="one CYPHER prefix"):
        graph.cypher("CYPHER 5 CYPHER 25 RETURN 1")


def test_graphbridge_prefixed_statements_reach_the_engine(graph):
    graph.cypher("CREATE (:L {id: 1})-[:E]->(:L {id: 2})")
    rows = list(graph.cypher("CYPHER 5 MATCH (a:`L` {id: 1})-[r*1..3]->(b:L) RETURN b.id AS id"))
    assert [r["id"] for r in rows] == [2]
    # The fulltext procedure itself is a later phase; the prefix must not be
    # what fails.
    with pytest.raises(kglite.KgError) as info:
        graph.cypher("CYPHER 25 CALL db.index.fulltext.queryNodes() YIELD node RETURN node")
    assert "Unsupported Cypher version" not in str(info.value)
