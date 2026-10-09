"""Rules that demand something be present are judged when a transaction ends.

A required relationship, a minimum degree, an enforced inverse, a symmetric
partner and a stored transitive closure are satisfiable across statements, so a
node created in one statement may receive its relationship in the next. A lone
Cypher statement or bulk call is its own transaction; inside ``begin()`` the
verdict is the commit's, a refusal rolls the whole transaction back, and the
graph is left as it was."""

import pandas as pd
import pytest

import kglite

STORAGES = ["memory", "mapped", "disk"]


def make_graph(storage, tmp_path, relationships, classes=None):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.define_ontology({"classes": classes or {"Person": {}, "Company": {}, "Node": {}}, "relationships": relationships})
    return g


def works_at(**extra):
    return {"WORKS_AT": {"domain": "Person", "range": "Company", "enforcement": "error", **extra}}


def count(g, label="Person"):
    return g.cypher(f"MATCH (n:{label}) RETURN count(n) AS c").to_list()[0]["c"]


@pytest.mark.parametrize("storage", STORAGES)
def test_a_node_and_its_edge_in_later_statements_commit(storage, tmp_path):
    g = make_graph(storage, tmp_path, works_at(required=True))
    g.cypher("CREATE (:Company {id: 7})")
    with g.begin() as tx:
        tx.cypher("CREATE (:Person {id: 1})")
        tx.cypher("MATCH (p:Person {id: 1}), (c:Company {id: 7}) CREATE (p)-[:WORKS_AT]->(c)")
    assert count(g) == 1


@pytest.mark.parametrize("storage", STORAGES)
def test_an_unmet_rule_at_commit_refuses_and_rolls_the_transaction_back(storage, tmp_path):
    g = make_graph(storage, tmp_path, works_at(required=True))
    g.cypher("CREATE (:Company {id: 7})")
    tx = g.begin()
    tx.cypher("CREATE (:Person {id: 1})")
    tx.cypher("CREATE (:Person {id: 2})-[:WORKS_AT]->(:Company {id: 8})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        tx.commit()
    assert info.value.rule == "required_relationship"
    assert info.value.entity == "relationship"
    assert info.value.entity_type == "WORKS_AT"
    assert count(g) == 0 and count(g, "Company") == 1, "the complete half of the transaction rolled back too"


@pytest.mark.parametrize("storage", STORAGES)
def test_a_lone_statement_and_a_bulk_call_are_their_own_transaction(storage, tmp_path):
    g = make_graph(storage, tmp_path, works_at(required=True))
    g.cypher("CREATE (:Person {id: 1})-[:WORKS_AT]->(:Company {id: 7})")
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("CREATE (:Person {id: 2})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.add_nodes(pd.DataFrame({"id": [3, 4]}), "Person", "id")
    assert info.value.rule == "required_relationship"
    assert count(g) == 1
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("MATCH (:Person {id: 1})-[r:WORKS_AT]->() DELETE r")
    assert g.cypher("MATCH ()-[r:WORKS_AT]->() RETURN count(r) AS c").to_list() == [{"c": 1}]


@pytest.mark.parametrize("storage", STORAGES)
def test_a_minimum_degree(storage, tmp_path):
    g = make_graph(storage, tmp_path, works_at(cardinality={"min": 2}))
    g.cypher("CREATE (:Company {id: 7}), (:Company {id: 8})")
    tx = g.begin()
    tx.cypher("CREATE (:Person {id: 1})")
    tx.cypher("MATCH (p:Person {id: 1}), (c:Company {id: 7}) CREATE (p)-[:WORKS_AT]->(c)")
    with pytest.raises(kglite.OntologyViolationError) as info:
        tx.commit()
    assert info.value.rule == "min_cardinality"
    with g.begin() as tx:
        tx.cypher("CREATE (:Person {id: 1})")
        tx.cypher("MATCH (p:Person {id: 1}), (c:Company) CREATE (p)-[:WORKS_AT]->(c)")
    assert count(g) == 1


@pytest.mark.parametrize("storage", STORAGES)
def test_inverse_and_symmetric_pairs(storage, tmp_path):
    g = make_graph(
        storage,
        tmp_path,
        {
            "PARENT_OF": {"inverse_name": "CHILD_OF", "inverse_enforced": True, "enforcement": "error"},
            "CHILD_OF": {},
            "KNOWS": {"symmetric": True, "enforcement": "error"},
        },
    )
    g.cypher("CREATE (:Node {id: 1}), (:Node {id: 2})")
    with g.begin() as tx:
        tx.cypher("MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (a)-[:PARENT_OF]->(b), (a)-[:KNOWS]->(b)")
        tx.cypher("MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (b)-[:CHILD_OF]->(a), (b)-[:KNOWS]->(a)")
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.cypher("MATCH (:Node {id: 2})-[r:CHILD_OF]->() DELETE r")
    assert info.value.rule == "inverse"
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.cypher("MATCH (:Node {id: 2})-[r:KNOWS]->() DELETE r")
    assert info.value.rule == "symmetric"


@pytest.mark.parametrize("storage", STORAGES)
def test_a_stored_transitive_closure(storage, tmp_path):
    g = make_graph(storage, tmp_path, {"BELOW": {"transitive": True, "enforcement": "error"}})
    g.cypher("CREATE (:Node {id: 1}), (:Node {id: 2}), (:Node {id: 3})")
    link = "MATCH (a:Node {{id: {}}}), (b:Node {{id: {}}}) CREATE (a)-[:BELOW]->(b)"
    tx = g.begin()
    tx.cypher(link.format(1, 2))
    tx.cypher(link.format(2, 3))
    with pytest.raises(kglite.OntologyViolationError) as info:
        tx.commit()
    assert info.value.rule == "transitive"
    with g.begin() as tx:
        for pair in [(1, 2), (2, 3), (1, 3)]:
            tx.cypher(link.format(*pair))
    assert g.cypher("MATCH ()-[r:BELOW]->() RETURN count(r) AS c").to_list() == [{"c": 3}]


@pytest.mark.parametrize("storage", STORAGES)
def test_declaring_over_violating_data_is_refused(storage, tmp_path):
    g = make_graph(storage, tmp_path, {})
    g.cypher("CREATE (:Person {id: 1}), (:Person {id: 2}), (:Company {id: 7})")
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.define_ontology({"classes": {"Person": {}, "Company": {}}, "relationships": works_at(required=True)})
    assert info.value.report[0]["rule"] == "required_relationship"
    assert info.value.report[0]["count"] == 2
    warn = works_at(required=True, enforcement="warn")
    warnings = g.define_ontology({"classes": {"Person": {}, "Company": {}}, "relationships": warn})
    assert any("required_relationship" in w for w in warnings), warnings


@pytest.mark.parametrize("storage", STORAGES)
def test_warn_reports_at_the_commit(storage, tmp_path):
    g = make_graph(storage, tmp_path, works_at(required=True, enforcement="warn"))
    tx = g.begin()
    tx.cypher("CREATE (:Person {id: 1}), (:Person {id: 2})")
    with pytest.warns(UserWarning, match=r"ontology warning \(required_relationship\): 2 nodes"):
        tx.commit()
    assert count(g) == 2


@pytest.mark.parametrize("storage", STORAGES)
def test_the_audit_counts_min_cardinality_when_the_relationship_type_is_absent(storage, tmp_path):
    g = make_graph(storage, tmp_path, works_at(required=True, cardinality={"min": 1}, enforcement="warn"))
    g.cypher("CREATE (:Person {id: 1}), (:Person {id: 2}), (:Company {id: 7})")
    rows = {
        r["rule"]: r["violations"]
        for r in g.cypher("CALL ontology_audit() YIELD rule, violations RETURN rule, violations").to_list()
    }
    assert rows["WORKS_AT.required"] == 2
    assert rows["WORKS_AT.cardinality"] == 2


@pytest.mark.parametrize("storage", STORAGES)
def test_declaring_min_cardinality_at_error_over_absent_type_is_refused_and_the_gate_agrees(storage, tmp_path):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.cypher("CREATE (:Person {id: 1}), (:Person {id: 2}), (:Company {id: 7})")
    with pytest.raises(Exception):
        g.define_ontology({"classes": {"Person": {}, "Company": {}}, "relationships": works_at(cardinality={"min": 1})})
    h = make_graph(storage, tmp_path / "h", works_at(cardinality={"min": 1}))
    with pytest.raises(Exception):
        h.cypher("CREATE (:Person {id: 1})")
