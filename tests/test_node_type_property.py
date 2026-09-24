"""A node property named `type` is an ordinary property on every write path.

`CREATE`/`MERGE` maps always wrote it, but `SET n.type`, `SET n += {type: …}`,
`SET n = {…}`, `ON CREATE SET` and `REMOVE n.type` refused ("Cannot SET node
type via property assignment") — so a Neo4j importer that runs
`SET e.type = ent.type` loaded no entities at all. The rule is the
relationship side's: a stored `type` wins on read, the label answers
otherwise, and `labels(n)` never changes. `node_type` and `label` are the same
soft aliases and follow the same rule.
"""

from __future__ import annotations

import pytest

import kglite


def _graph(mode: str, tmp_path) -> kglite.KnowledgeGraph:
    if mode == "memory":
        return kglite.KnowledgeGraph()
    if mode == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "disk.kgl"))


MODES = ["memory", "mapped", "disk"]


def _one(graph, query, **params):
    rows = graph.cypher(query, params=params).to_list()
    assert len(rows) == 1, rows
    return rows[0]


@pytest.mark.parametrize("mode", MODES)
def test_every_write_path_writes_the_type_property(mode, tmp_path):
    g = _graph(mode, tmp_path)
    assert _one(g, "CREATE (e:Entity {id: 'a', type: 'person'}) RETURN e.type AS t, labels(e) AS l") == {
        "t": "person",
        "l": ["Entity"],
    }
    assert _one(g, "MATCH (e:Entity {id: 'a'}) SET e.type = 'concept' RETURN e.type AS t") == {"t": "concept"}
    assert _one(g, "MATCH (e:Entity {id: 'a'}) SET e += {type: 'org'} RETURN e.type AS t") == {"t": "org"}
    assert _one(g, "MERGE (e:Entity {id: 'b'}) ON CREATE SET e.type = 'new' RETURN e.type AS t") == {"t": "new"}
    assert _one(g, "MERGE (e:Entity {id: 'b'}) ON MATCH SET e.type = 'seen' RETURN e.type AS t") == {"t": "seen"}
    assert _one(g, "CREATE (e:Entity {id: 'c'}) SET e.type = $t RETURN e.type AS t", t="param") == {"t": "param"}
    assert _one(g, "MATCH (e:Entity {id: 'c'}) SET e = {type: 'replaced', note: 'n'} RETURN e.type AS t") == {
        "t": "replaced"
    }
    rows = g.cypher("MATCH (e:Entity) RETURN e.id AS id, e.type AS t, labels(e) AS l ORDER BY id").to_list()
    assert rows == [
        {"id": "a", "t": "org", "l": ["Entity"]},
        {"id": "b", "t": "seen", "l": ["Entity"]},
        {"id": "c", "t": "replaced", "l": ["Entity"]},
    ]
    # The label is untouched: MATCH by label still finds all three, none by the value.
    assert _one(g, "MATCH (e:Entity) RETURN count(e) AS n") == {"n": 3}
    assert g.cypher("MATCH (e:org) RETURN e").to_list() == []
    assert _one(g, "MATCH (e:Entity) WHERE e.type = 'org' RETURN e.id AS id") == {"id": "a"}


@pytest.mark.parametrize("mode", MODES)
def test_remove_type_falls_back_to_the_label(mode, tmp_path):
    g = _graph(mode, tmp_path)
    g.cypher("CREATE (:Entity {id: 'a', type: 'person'})")
    assert _one(g, "MATCH (e:Entity {id: 'a'}) REMOVE e.type RETURN e.type AS t, labels(e) AS l") == {
        "t": "Entity",
        "l": ["Entity"],
    }
    assert _one(g, "MATCH (e:Entity {id: 'a'}) SET e.type = null RETURN e.type AS t") == {"t": "Entity"}
    assert _one(g, "MATCH (e:Entity {id: 'a'}) RETURN labels(e) AS l") == {"l": ["Entity"]}


@pytest.mark.parametrize("alias", ["node_type", "label"])
def test_the_other_soft_aliases_write_the_same_way(alias):
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Entity {id: 'a'})")
    assert _one(g, f"MATCH (e:Entity) RETURN e.{alias} AS t") == {"t": "Entity"}
    assert _one(g, f"MATCH (e:Entity) SET e.{alias} = 'x' RETURN e.{alias} AS t, labels(e) AS l") == {
        "t": "x",
        "l": ["Entity"],
    }
    assert _one(g, f"MATCH (e:Entity) REMOVE e.{alias} RETURN e.{alias} AS t") == {"t": "Entity"}


def test_the_type_property_survives_save_and_load(tmp_path):
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Entity {id: 'a'})")
    g.cypher("MATCH (e:Entity) SET e.type = 'person'")
    path = str(tmp_path / "g.kgl")
    g.save(path)
    loaded = kglite.load(path)
    assert _one(loaded, "MATCH (e:Entity) RETURN e.type AS t, labels(e) AS l") == {"t": "person", "l": ["Entity"]}


def test_the_id_guard_stays():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Entity {id: 'a'})")
    with pytest.raises(kglite.CypherExecutionError, match="Cannot SET node id"):
        g.cypher("MATCH (e:Entity) SET e.id = 'b'")


def test_a_neo4j_style_entity_import_loads():
    """knwler's Neo4j importer statement, verbatim in shape."""
    g = kglite.KnowledgeGraph()
    batch = [
        {"id": "e1", "name": "Ada", "type": "person", "description": "a person"},
        {"id": "e2", "name": "ACME", "type": "org", "description": "a company"},
    ]
    g.cypher(
        "UNWIND $batch AS ent MERGE (e:Entity {id: ent.id}) "
        "SET e.description = ent.description, e.name = ent.name, e.type = ent.type",
        params={"batch": batch},
    )
    rows = g.cypher("MATCH (e:Entity) RETURN e.id AS id, e.type AS t ORDER BY id").to_list()
    assert rows == [{"id": "e1", "t": "person"}, {"id": "e2", "t": "org"}]
