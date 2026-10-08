"""A title the engine mints never satisfies a required ``name`` or ``title``.

``CREATE (:Person {age: 3})`` gets the title ``Person_<id>``, and ``n.name``
falls back to the title on reads. A write gate that took that fallback as a
supplied value let a Person without a name through (issue #222, criterion 3).
"""

import pandas as pd
import pytest

import kglite

STORAGES = ["memory", "mapped", "disk"]
ONTOLOGY = {
    "classes": {
        "Person": {
            "required_properties": ["name"],
            "property_types": {"name": "string"},
            "enforcement": "error",
        }
    }
}


def _graph(storage, tmp_path):
    opts = {} if storage == "memory" else {"storage": storage}
    if storage == "disk":
        opts["path"] = str(tmp_path / "disk")
    g = kglite.KnowledgeGraph(**opts)
    g.define_ontology(ONTOLOGY)
    return g


def _people(g):
    return g.cypher("MATCH (n:Person) RETURN count(n) AS c").to_list()[0]["c"]


@pytest.mark.parametrize("storage", STORAGES)
def test_create_without_a_name_is_refused(storage, tmp_path):
    g = _graph(storage, tmp_path)
    with pytest.raises(kglite.OntologyViolationError) as err:
        g.cypher("CREATE (:Person {age: 3})")
    assert "required_property" in str(err.value)
    assert "'name'" in str(err.value)
    assert _people(g) == 0
    g.cypher("CREATE (:Person {name: 'Ada'})")
    assert _people(g) == 1


@pytest.mark.parametrize("storage", STORAGES)
def test_merge_and_remove_cannot_drop_a_name(storage, tmp_path):
    g = _graph(storage, tmp_path)
    g.cypher("CREATE (:Person {name: 'Ada', age: 1})")
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("MERGE (:Person {age: 3})")
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("MATCH (n:Person) REMOVE n.name")
    assert g.cypher("MATCH (n:Person) RETURN n.name AS n").to_list() == [{"n": "Ada"}]


@pytest.mark.parametrize("storage", STORAGES)
def test_set_to_null_cannot_drop_a_name(storage, tmp_path):
    g = _graph(storage, tmp_path)
    g.cypher("CREATE (:Person {name: 'Ada'})")
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("MATCH (n:Person) SET n.name = null")
    assert g.cypher("MATCH (n:Person) RETURN n.name AS n").to_list() == [{"n": "Ada"}]


@pytest.mark.parametrize("storage", STORAGES)
def test_add_nodes_without_a_name_column_is_refused(storage, tmp_path):
    g = _graph(storage, tmp_path)
    df = pd.DataFrame({"pid": [1, 2], "age": [3, 4]})
    with pytest.raises(kglite.OntologyViolationError) as err:
        g.add_nodes(df, "Person", "pid")
    assert "required_property" in str(err.value)
    assert _people(g) == 0
    ok = pd.DataFrame({"pid": [1, 2], "name": ["Ada", "Bo"]})
    g.add_nodes(ok, "Person", "pid", "name")
    assert _people(g) == 2


@pytest.mark.parametrize("storage", STORAGES)
def test_a_declared_title_field_must_be_supplied(storage, tmp_path):
    g = _graph(storage, tmp_path)
    ok = pd.DataFrame({"pid": [1], "name": ["Ada"], "age": [1]})
    g.add_nodes(ok, "Person", "pid", "name")
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("CREATE (:Person {age: 3})")
    assert _people(g) == 1


def test_a_required_title_property_needs_a_supplied_title(tmp_path):
    g = kglite.KnowledgeGraph()
    g.define_ontology({"classes": {"Doc": {"required_properties": ["title"], "enforcement": "error"}}})
    with pytest.raises(kglite.OntologyViolationError):
        g.cypher("CREATE (:Doc {pages: 3})")
    g.cypher("CREATE (:Doc {title: 'T', pages: 3})")
    assert g.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list()[0]["c"] == 1
