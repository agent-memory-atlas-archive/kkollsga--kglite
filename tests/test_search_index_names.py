"""Named full-text and vector indexes: `build_text_index(name=)`,
`build_vector_index(name=)` and the `name` key of the build procedures, the
`SHOW INDEXES` row a Neo4j search client preflights, and `DROP INDEX <name>`
over a search structure.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import kglite
from kglite import KnowledgeGraph

HEAP = ["memory", "mapped"]  # a disk graph has no BM25 index
PREFLIGHT = (
    "SHOW INDEXES YIELD name, state, type, entityType, labelsOrTypes, properties, indexProvider, options "
    "WHERE name = $index RETURN name, state, type, entityType, labelsOrTypes, properties, indexProvider, options "
    "LIMIT 2"
)


def _rows(graph: KnowledgeGraph, query: str = "SHOW INDEXES", **params) -> list[dict]:
    return graph.cypher(query, params=params or None).to_list()


def _names(graph: KnowledgeGraph) -> list[tuple[str, str]]:
    return sorted((r["name"], r["type"]) for r in _rows(graph))


def _search_graph(mode: str, tmp_path: Path) -> KnowledgeGraph:
    graph = kglite.open(str(tmp_path / "g"), storage="disk") if mode == "disk" else KnowledgeGraph(storage=mode)
    for i, text in enumerate(["quick fox", "quick quick dog", "lazy cat"]):
        graph.cypher("CREATE (:Doc {id: $i, text: $text})", params={"i": i, "text": text})
    graph.add_embeddings("Doc", "text", {0: [1.0, 0.0, 0.0], 1: [0.0, 1.0, 0.0], 2: [0.0, 0.0, 1.0]})
    return graph


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_a_named_vector_index_answers_the_preflight_row(mode, tmp_path):
    graph = _search_graph(mode, tmp_path)
    graph.build_vector_index("Doc", "text", name="doc_vec")
    [row] = _rows(graph, PREFLIGHT, index="doc_vec")
    assert row == {
        "name": "doc_vec",
        "state": "ONLINE",
        "type": "VECTOR",
        "entityType": "NODE",
        "labelsOrTypes": ["Doc"],
        "properties": ["text"],
        "indexProvider": "vector-2.0",
        "options": {"indexConfig": {"vector.dimensions": 3, "vector.similarity_function": "COSINE"}},
    }
    hits = graph.cypher(
        "CALL db.index.vector.queryNodes($index, 2, $q) YIELD node AS entity, score RETURN entity.id AS id",
        params={"index": "doc_vec", "q": [0.0, 1.0, 0.0]},
    ).to_list()
    assert hits[0]["id"] == 1
    # The canonical name keeps resolving.
    assert _rows(graph, PREFLIGHT, index="Doc.text") == []  # listed under the registered name only
    again = graph.cypher(
        "CALL db.index.vector.queryNodes('Doc.text', 1, $q) YIELD node RETURN node.id AS id",
        params={"q": [0.0, 1.0, 0.0]},
    ).to_list()
    assert again == [{"id": 1}]


@pytest.mark.parametrize("mode", HEAP)
def test_a_named_fulltext_index_answers_the_preflight_row_and_queries(mode, tmp_path):
    graph = _search_graph(mode, tmp_path)
    graph.build_text_index("Doc", "text", name="doc_ft")
    [row] = _rows(graph, PREFLIGHT, index="doc_ft")
    assert row["type"] == "FULLTEXT" and row["state"] == "ONLINE" and row["entityType"] == "NODE"
    assert row["labelsOrTypes"] == ["Doc"] and row["properties"] == ["text"]
    assert row["indexProvider"] == "fulltext-1.0"
    config = row["options"]["indexConfig"]
    assert config["fulltext.analyzer"] and config["fulltext.eventually_consistent"] is True
    hits = graph.cypher(
        "CALL db.index.fulltext.queryNodes($index, $q, {limit: $limit}) YIELD node AS entity, score "
        "RETURN entity.id AS id, score ORDER BY score DESC LIMIT $limit",
        params={"index": "doc_ft", "q": "quick", "limit": 5},
    ).to_list()
    assert [h["id"] for h in hits] == [1, 0]


@pytest.mark.parametrize("mode", HEAP)
def test_dropping_a_search_name_drops_only_that_structure(mode, tmp_path):
    graph = _search_graph(mode, tmp_path)
    graph.build_text_index("Doc", "text", name="doc_ft")
    graph.build_vector_index("Doc", "text", name="doc_vec")
    graph.cypher("DROP INDEX doc_vec")
    assert _names(graph) == [("doc_ft", "FULLTEXT")]
    assert graph.has_text_index("Doc", "text") and not graph.has_vector_index("Doc", "text")


def test_a_taken_search_name_refuses_the_build_and_builds_nothing():
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {text: 'quick fox'})")
    graph.cypher("CREATE CONSTRAINT dup FOR (n:Doc) REQUIRE n.text IS UNIQUE")
    with pytest.raises(ValueError, match="constraint"):
        graph.build_text_index("Doc", "text", name="dup")
    assert not graph.has_text_index("Doc", "text")
    graph.build_text_index("Doc", "text", name="doc_ft")
    graph.build_text_index("Doc", "text", name="doc_ft")  # a rebuild under the same name is fine
    graph.cypher("CREATE (:Other {text: 'x'})")
    with pytest.raises(ValueError, match="already exists"):
        graph.build_text_index("Other", "text", name="doc_ft")


def test_procedures_take_a_name_for_node_and_relationship_indexes():
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1}), (:Doc {id: 2})")
    graph.cypher("MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {k: 0, text: 'quick fox'}]->(b)")
    graph.cypher("CALL db.relationship_text_index.build({type: 'CLAIMS', text_column: 'text', name: 'claims_ft'})")
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() CALL db.relationship_embeddings.set({type: 'CLAIMS', text_column: 'text', "
        "entries: [{relationship: r, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
    )
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type: 'CLAIMS', text_column: 'text', name: 'claims_vec'})"
    )
    rows = {r["name"]: r for r in _rows(graph)}
    assert rows["claims_ft"]["type"] == "FULLTEXT" and rows["claims_ft"]["entityType"] == "RELATIONSHIP"
    assert rows["claims_vec"]["type"] == "VECTOR" and rows["claims_vec"]["labelsOrTypes"] == ["CLAIMS"]
    hit = graph.cypher(
        "CALL db.index.fulltext.queryRelationships('claims_ft', 'quick') YIELD relationship, score "
        "RETURN relationship.k AS k"
    ).to_list()
    assert hit == [{"k": 0}]
    graph.cypher("DROP INDEX claims_vec")
    assert [r["name"] for r in _rows(graph)] == ["claims_ft"]
    with pytest.raises(Exception, match="already exists"):
        graph.cypher("CALL db.node_text_index.build({type: 'Doc', text_column: 'id', name: 'claims_ft'})")


@pytest.mark.parametrize("mode", HEAP)
def test_search_names_survive_save_and_load(mode, tmp_path):
    graph = _search_graph(mode, tmp_path)
    graph.build_text_index("Doc", "text", name="doc_ft")
    graph.build_vector_index("Doc", "text", name="doc_vec")
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    reloaded = kglite.load(path)
    assert _names(reloaded) == [("doc_ft", "FULLTEXT"), ("doc_vec", "VECTOR")]
    assert [r["name"] for r in _rows(reloaded, PREFLIGHT, index="doc_vec")] == ["doc_vec"]
    reloaded.cypher("DROP INDEX doc_ft")
    assert _names(reloaded) == [("doc_vec", "VECTOR")]
