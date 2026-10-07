"""Cross-storage golden semantics for shared scalar execution paths."""

import datetime as dt

import pytest

import kglite

pytestmark = pytest.mark.parity


@pytest.fixture(params=["memory", "mapped", "disk"])
def scalar_graph(request, tmp_path):
    mode = request.param
    if mode == "memory":
        return kglite.KnowledgeGraph()
    if mode == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "scalar-disk"))


def test_range_temporal_duration_and_regex_golden(scalar_graph):
    query = (
        "WITH duration({months: 2, days: 3}) * 2 AS d "
        "RETURN range(-2, 2) AS r, "
        "add_years(date('2024-02-29'), 1) AS shifted, "
        "d.months AS months, d.days AS days, "
        "'Alpha42' =~ '^Alpha[0-9]+$' AS regex_op, "
        "text_match_regex('Alpha42', '^Alpha[0-9]+$') AS regex_fn"
    )
    expected = {
        "r": [-2, -1, 0, 1, 2],
        "shifted": dt.date(2025, 2, 28),
        "months": 4,
        "days": 6,
        "regex_op": True,
        "regex_fn": True,
    }
    assert scalar_graph.cypher(query).to_list() == [expected]
    assert scalar_graph.cypher(query, disable_optimizer=True).to_list() == [expected]


def _rows(graph, query, **kwargs):
    return graph.cypher(query, **kwargs).to_list()


def test_merge_yields_one_row_per_matching_relationship(scalar_graph):
    _rows(scalar_graph, "CREATE (a:A {id: 1}), (b:B {id: 2}), (a)-[:E {k: 1}]->(b), (a)-[:E {k: 2}]->(b)")
    query = "MATCH (a:A), (b:B) MERGE (a)-[r:E]->(b) RETURN r.k AS k ORDER BY k"
    expected = [{"k": 1}, {"k": 2}]
    assert _rows(scalar_graph, query) == expected
    assert _rows(scalar_graph, query, disable_optimizer=True) == expected
    # A pattern that names a property still narrows to the members carrying it.
    assert _rows(scalar_graph, "MATCH (a:A), (b:B) MERGE (a)-[r:E {k: 2}]->(b) RETURN r.k AS k") == [{"k": 2}]
    assert _rows(scalar_graph, "MATCH ()-[r:E]->() RETURN count(r) AS c") == [{"c": 2}]


def test_merge_on_match_runs_for_every_matching_relationship(scalar_graph):
    _rows(scalar_graph, "CREATE (a:A {id: 1}), (b:B {id: 2}), (a)-[:E]->(b), (a)-[:E]->(b)")
    rows = _rows(
        scalar_graph,
        "MATCH (a:A), (b:B) MERGE (a)-[r:E]->(b) ON MATCH SET r.seen = true ON CREATE SET r.seen = false "
        "RETURN count(r) AS c, count(r.seen) AS seen",
    )
    assert rows == [{"c": 2, "seen": 2}]
    assert _rows(scalar_graph, "MATCH ()-[r:E]->() WHERE r.seen = true RETURN count(r) AS c") == [{"c": 2}]


def test_merge_yields_one_row_per_matching_node(scalar_graph):
    _rows(scalar_graph, "CREATE (:P {id: 1, g: 'x'}), (:P {id: 2, g: 'x'}), (:P {id: 3, g: 'y'})")
    query = "MATCH (a:P {id: 3}) MERGE (b:P {g: 'x'}) ON MATCH SET b.hit = true RETURN b.id AS id ORDER BY id"
    assert _rows(scalar_graph, query) == [{"id": 1}, {"id": 2}]
    assert _rows(scalar_graph, "MATCH (n:P) WHERE n.hit = true RETURN count(n) AS c") == [{"c": 2}]
    # No match still creates exactly once per input row, binding the new node.
    created = _rows(scalar_graph, "MATCH (a:P) MERGE (b:P {g: 'z'}) ON CREATE SET b.fresh = true RETURN count(b) AS c")
    assert created == [{"c": 3}]
    assert _rows(scalar_graph, "MATCH (n:P {g: 'z'}) RETURN count(n) AS c") == [{"c": 1}]


def test_merge_unlabelled_pairs_every_input_row_with_every_match(scalar_graph):
    _rows(scalar_graph, "CREATE (), ()")
    assert _rows(scalar_graph, "MATCH (a) MERGE (b) RETURN count(*) AS c") == [{"c": 4}]
    assert _rows(scalar_graph, "MATCH (n) RETURN count(n) AS c") == [{"c": 2}]
