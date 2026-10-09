"""Maximum cardinality at write time: a source of the declared domain holding
more outgoing relationships of the type than `max` is refused (`error`) or
reported (`warn`) with rule `cardinality`; the minimum stays audit-only."""

import pandas as pd
import pytest

import kglite


def declaration(severity="error", card=None):
    return {
        "classes": {"Person": {}},
        "relationships": {
            "KNOWS": {
                "domain": "Person",
                "range": "Person",
                "cardinality": card or {"max": 2},
                "enforcement": severity,
            }
        },
    }


def make_graph(storage, tmp_path, severity="error"):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.cypher("CREATE (:Person {id: 1}), (:Person {id: 2}), (:Person {id: 3}), (:Person {id: 4})")
    g.define_ontology(declaration(severity))
    return g


def befriend(g, source, target):
    return g.cypher(f"MATCH (a:Person {{id: {source}}}), (b:Person {{id: {target}}}) CREATE (a)-[:KNOWS]->(b)")


def knows(g):
    return g.cypher("MATCH ()-[r:KNOWS]->() RETURN count(r) AS c").to_list()[0]["c"]


def frame(rows):
    return pd.DataFrame(rows, columns=["s", "t"])


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_cypher_create_over_the_maximum_is_refused(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    befriend(g, 1, 2)
    befriend(g, 1, 3)
    with pytest.raises(kglite.OntologyViolationError) as info:
        befriend(g, 1, 4)
    assert info.value.rule == "cardinality"
    assert info.value.entity == "relationship"
    assert info.value.entity_type == "KNOWS"
    assert knows(g) == 2


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_bulk_loader_over_the_maximum_writes_nothing(storage, tmp_path):
    g = make_graph(storage, tmp_path)
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.add_connections(frame([(1, 2), (1, 3), (1, 4)]), "KNOWS", "Person", "s", "Person", "t")
    assert info.value.rule == "cardinality"
    assert knows(g) == 0
    g.add_connections(frame([(1, 2), (1, 3)]), "KNOWS", "Person", "s", "Person", "t")
    assert knows(g) == 2
    # A repeated pair merges into the stored edge and adds nothing.
    g.add_connections(frame([(1, 2)]), "KNOWS", "Person", "s", "Person", "t")
    assert knows(g) == 2


def test_warn_reports_and_the_write_lands(tmp_path):
    g = make_graph("memory", tmp_path, "warn")
    befriend(g, 1, 2)
    befriend(g, 1, 3)
    result = befriend(g, 1, 4)
    assert any("ontology warning (cardinality)" in w for w in result.diagnostics["warnings"])
    assert knows(g) == 3


def test_declaring_an_error_maximum_over_violating_data_is_refused():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (a:Person {id: 1}), (b:Person {id: 2}), (c:Person {id: 3})")
    for target in (2, 3):
        befriend(g, 1, target)
    with pytest.raises(kglite.OntologyViolationError) as info:
        g.define_ontology(declaration("error", {"max": 1}))
    assert info.value.report[0]["rule"] == "cardinality"
    assert info.value.report[0]["count"] == 1
    # The minimum alone never refuses a declaration or a write.
    g.define_ontology(declaration("error", {"min": 5}))
    befriend(g, 2, 3)
