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


def test_path_functions_accept_a_path_carried_as_a_value(scalar_graph):
    _rows(scalar_graph, "CREATE (:A {id: 1})-[:T]->(:B {id: 2})")
    query = (
        "MATCH p = (:A)-[:T]->(:B) RETURN "
        "[x IN [p] | [n IN nodes(x) | n.id]] AS ids, "
        "[x IN [p] | size(relationships(x))] AS rels, "
        "[x IN [p] | length(x)] AS hops, "
        "[x IN [p] | type(relationships(x)[0])] AS types"
    )
    expected = [{"ids": [[1, 2]], "rels": [1], "hops": [1], "types": ["T"]}]
    assert _rows(scalar_graph, query) == expected
    assert _rows(scalar_graph, query, disable_optimizer=True) == expected
    carried = "MATCH p = (:A)-[:T]->(:B) UNWIND [p] AS q RETURN size(nodes(q)) AS n, size(relationships(q)) AS r"
    assert _rows(scalar_graph, carried) == [{"n": 2, "r": 1}]


def test_label_predicates_cover_relationships_and_null(scalar_graph):
    _rows(scalar_graph, "CREATE (:A {id: 1})-[:T]->(:B {id: 2}), (:A {id: 3})")
    rel = "MATCH (:A)-[r]->(:B) RETURN r:T AS is_t, r:U AS is_u"
    assert _rows(scalar_graph, rel) == [{"is_t": True, "is_u": False}]
    assert _rows(scalar_graph, rel, disable_optimizer=True) == [{"is_t": True, "is_u": False}]
    optional = (
        "MATCH (a:A) OPTIONAL MATCH (a)-[:T]->(m:B) RETURN a.id AS id, m:B AS has_b, NOT m:B AS not_b ORDER BY id"
    )
    expected = [{"id": 1, "has_b": True, "not_b": False}, {"id": 3, "has_b": None, "not_b": None}]
    assert _rows(scalar_graph, optional) == expected
    assert _rows(scalar_graph, "MATCH (a:A) OPTIONAL MATCH (a)-[:T]->(m) WHERE m:B RETURN count(m) AS c") == [{"c": 1}]
    assert _rows(scalar_graph, "MATCH ()-[r:T]->() WHERE r:T RETURN count(r) AS c") == [{"c": 1}]
    assert _rows(scalar_graph, "MATCH ()-[r:T]->() WHERE r:U RETURN count(r) AS c") == [{"c": 0}]


def test_string_predicates_on_non_strings_are_null(scalar_graph):
    query = (
        "RETURN 1 CONTAINS 'a' AS c1, 'a' CONTAINS 1 AS c2, [1] STARTS WITH 'a' AS s1, "
        "1 ENDS WITH 'a' AS e1, 'abc' CONTAINS 'b' AS ok, null CONTAINS 'a' AS n"
    )
    expected = {"c1": None, "c2": None, "s1": None, "e1": None, "ok": True, "n": None}
    assert _rows(scalar_graph, query) == [expected]
    assert _rows(scalar_graph, query, disable_optimizer=True) == [expected]
    # A null predicate keeps no row in either direction.
    assert _rows(scalar_graph, "UNWIND [1, 'a'] AS v WITH v WHERE v CONTAINS 'a' RETURN v") == [{"v": "a"}]
    assert _rows(scalar_graph, "UNWIND [1, 'a'] AS v WITH v WHERE NOT v CONTAINS 'a' RETURN v") == []


def test_properties_of_a_map_is_the_map(scalar_graph):
    query = "RETURN properties({a: 1, b: 'x'}) AS m, properties(null) AS n"
    assert _rows(scalar_graph, query) == [{"m": {"a": 1, "b": "x"}, "n": None}]


def test_deleted_relationship_keeps_its_type_within_the_statement(scalar_graph):
    _rows(scalar_graph, "CREATE (:A {id: 1})-[:T]->(:B {id: 2})")
    deleted = "MATCH (:A)-[r]->(:B) DELETE r RETURN type(r) AS t, r:T AS is_t"
    assert _rows(scalar_graph, deleted) == [{"t": "T", "is_t": True}]
    assert _rows(scalar_graph, "MATCH ()-[r]->() RETURN count(r) AS c") == [{"c": 0}]
    _rows(scalar_graph, "CREATE (:A {id: 5})-[:U]->(:B {id: 6})")
    carried = "MATCH (:A {id: 5})-[r]->() WITH r DELETE r RETURN type(r) AS t"
    assert _rows(scalar_graph, carried) == [{"t": "U"}]
    # A slot freed by the delete and reused by a CREATE keeps reporting the new type.
    reuse = "MATCH (a:A {id: 5})-[r]->() DELETE r CREATE (a)-[s:V]->(a) RETURN type(s) AS t"
    _rows(scalar_graph, "CREATE (:A {id: 5})-[:U]->(:B {id: 6})")
    assert _rows(scalar_graph, reuse) == [{"t": "V"}]
