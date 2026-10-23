"""GraphBridge's search preflight and query statements over `--neo4j-compat`.

A Neo4j search client names an index, checks its metadata with `SHOW INDEXES`,
and queries it with `CALL db.index.*`. The indexes are built here under a
name through the embedded API, saved, and served.
"""

from __future__ import annotations

import pytest

import kglite
from tests.conftest import (
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt]

neo4j = pytest.importorskip("neo4j")
from neo4j import GraphDatabase  # noqa: E402

COMPONENTS = "CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition LIMIT 5"
INDEXES = (
    "SHOW INDEXES YIELD name, state, type, entityType, labelsOrTypes, properties, indexProvider, options "
    "WHERE name = $index RETURN name, state, type, entityType, labelsOrTypes, properties, indexProvider, options "
    "LIMIT 2"
)
FULLTEXT = (
    "CYPHER 25 CALL db.index.fulltext.queryNodes($index,$query,{limit:$limit}) YIELD node AS entity,score "
    "RETURN entity,score ORDER BY score DESC LIMIT $limit"
)
FULLTEXT_RELATIONSHIPS = (
    "CYPHER 25 CALL db.index.fulltext.queryRelationships($index,$query,{limit:$limit}) "
    "YIELD relationship AS entity,score RETURN entity,score ORDER BY score DESC LIMIT $limit"
)
VECTOR = (
    "CYPHER 25 CALL db.index.vector.queryNodes($index,$limit,$query) YIELD node AS entity,score "
    "RETURN entity,score ORDER BY score DESC LIMIT $limit"
)
SEARCH_CLAUSE = (
    "CYPHER 25 MATCH (entity) SEARCH entity IN (VECTOR INDEX `doc_vec` FOR $query LIMIT $limit) "
    "SCORE AS score RETURN entity, score ORDER BY score DESC LIMIT $limit"
)


@pytest.fixture
def url(tmp_path):
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    graph = kglite.KnowledgeGraph()
    for i, text in enumerate(["quick fox", "quick quick dog", "lazy cat"]):
        graph.cypher("CREATE (:Doc {id: $i, text: $text})", params={"i": i, "text": text})
    graph.add_embeddings("Doc", "text", {0: [1.0, 0.0, 0.0], 1: [0.0, 1.0, 0.0], 2: [0.0, 0.0, 1.0]})
    graph.build_text_index("Doc", "text", name="doc_ft")
    graph.build_vector_index("Doc", "text", name="doc_vec")
    graph.cypher("MATCH (a:Doc {id: 0}), (b:Doc {id: 1}) CREATE (a)-[:CLAIMS {text: 'quick claim'}]->(b)")
    graph.cypher("CALL db.relationship_text_index.build({type: 'CLAIMS', text_column: 'text', name: 'claims_ft'})")
    path = tmp_path / "search.kgl"
    graph.save(str(path))
    proc, address = _spawn_bolt_server(path, extra_args=["--neo4j-compat"])
    try:
        yield address
    finally:
        _teardown_bolt_server(proc)


def _run(address, statement, **params):
    with GraphDatabase.driver(address, auth=None) as driver, driver.session() as session:
        return [record.data() for record in session.run(statement, params)]


def test_the_capability_preflight_passes(url):
    rows = _run(url, COMPONENTS)
    kernel = next(r for r in rows if r["name"] == "Neo4j Kernel")
    assert kernel["versions"][0].startswith("2026.") and kernel["edition"] in {"community", "enterprise"}
    assert "25" in next(r for r in rows if r["name"] == "Cypher")["versions"]


def test_the_index_preflight_returns_the_row_a_client_validates(url):
    [vector] = _run(url, INDEXES, index="doc_vec")
    assert (vector["state"], vector["type"], vector["entityType"]) == ("ONLINE", "VECTOR", "NODE")
    assert vector["labelsOrTypes"] == ["Doc"] and vector["properties"] == ["text"]
    assert vector["indexProvider"] == "vector-2.0"
    config = vector["options"]["indexConfig"]
    assert config["vector.dimensions"] == 3 and config["vector.similarity_function"].lower() == "cosine"
    [fulltext] = _run(url, INDEXES, index="doc_ft")
    assert fulltext["type"] == "FULLTEXT" and fulltext["indexProvider"] == "fulltext-1.0"
    assert fulltext["options"]["indexConfig"]["fulltext.eventually_consistent"] in (True, False)
    assert fulltext["options"]["indexConfig"]["fulltext.analyzer"]
    [relationship] = _run(url, INDEXES, index="claims_ft")
    assert (relationship["entityType"], relationship["labelsOrTypes"]) == ("RELATIONSHIP", ["CLAIMS"])
    assert _run(url, INDEXES, index="no_such_index") == []


def test_the_fulltext_statements_run(url):
    rows = _run(url, FULLTEXT, index="doc_ft", query="quick", limit=5)
    assert [r["entity"]["id"] for r in rows] == [1, 0]
    claims = _run(url, FULLTEXT_RELATIONSHIPS, index="claims_ft", query="quick", limit=5)
    assert len(claims) == 1 and claims[0]["entity"][1] == "CLAIMS"  # the driver renders a relationship as a tuple


def test_the_search_clause_returns_the_vector_procedure_rows(url):
    rows = _run(url, VECTOR, index="doc_vec", query=[0.0, 1.0, 0.0], limit=2)
    assert rows[0]["entity"]["id"] == 1
    searched = _run(url, SEARCH_CLAUSE, query=[0.0, 1.0, 0.0], limit=2)
    assert [r["entity"]["id"] for r in searched] == [r["entity"]["id"] for r in rows]
    assert [r["score"] for r in searched] == pytest.approx([r["score"] for r in rows])
