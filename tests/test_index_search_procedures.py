"""Neo4j-named index search: positional `CALL` arguments and the `db.index.*`
procedures over the indexes `SHOW INDEXES` lists.

Each procedure is checked against the engine call it names: the full-text rows
equal `text_bm25` ranked over the same index, the vector rows equal
`db.node_embeddings.query` / `db.relationship_embeddings.query`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]
EDGE_MODES = ["memory", "mapped"]  # BM25 indexes, node and relationship, refuse disk
DOCS = [
    (1, "alpha quick fox"),
    (2, "beta quick quick"),
    (3, "gamma ray burst"),
    (4, "delta quick quick quick"),
]
VECTORS = {1: [1.0, 0.0], 2: [0.0, 1.0], 3: [0.6, 0.8], 4: [0.8, 0.6]}
REL_VECTORS = {0: [1.0, 0.0], 1: [0.0, 1.0], 2: [0.6, 0.8]}
REL_TEXT = {0: "the quick brown fox", 1: "a quick marmoset", 2: "slow green turtles"}


def _graph(mode: str, tmp_path: Path) -> KnowledgeGraph:
    graph = kglite.open(str(tmp_path / "g"), storage="disk") if mode == "disk" else KnowledgeGraph(storage=mode)
    for doc_id, text in DOCS:
        graph.cypher("CREATE (:Doc {id: $id, text: $text})", params={"id": doc_id, "text": text})
    if mode != "disk":  # the BM25 index is heap-resident; disk graphs refuse it
        graph.build_text_index("Doc", "text")
    graph.add_embeddings("Doc", "text", VECTORS)
    graph.build_vector_index("Doc", "text", m=8)
    return graph


def _rel_graph(mode: str) -> KnowledgeGraph:
    graph = KnowledgeGraph(storage=mode)
    graph.cypher("CREATE (:Doc {id: 1}), (:Doc {id: 2})")
    for key, text in REL_TEXT.items():
        graph.cypher(
            "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {k: $k, text: $text}]->(b)",
            params={"k": key, "text": text},
        )
    graph.cypher("CALL db.relationship_text_index.build({type: 'CLAIMS', text_column: 'text'})")
    graph.cypher(
        "UNWIND $rows AS row MATCH ()-[r:CLAIMS {k: row.k}]->() "
        "CALL db.relationship_embeddings.set({type: 'CLAIMS', text_column: 'text', "
        "entries: [{relationship: r, vector: row.vector}]}) YIELD stored RETURN stored",
        params={"rows": [{"k": k, "vector": v} for k, v in REL_VECTORS.items()]},
    )
    graph.cypher("CALL db.relationship_embeddings.build_index({type: 'CLAIMS', text_column: 'text', m: 8})")
    return graph


# ── positional arguments ──────────────────────────────────────────────────


@pytest.mark.parametrize("mode", EDGE_MODES)
def test_fulltext_positional_equals_text_bm25_ranking(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    rows = graph.cypher(
        "CALL db.index.fulltext.queryNodes($index, $q, {limit: $limit}) "
        "YIELD node AS entity, score RETURN entity.id AS id, score ORDER BY score DESC LIMIT $limit",
        params={"index": "Doc.text", "q": "quick", "limit": 10},
    ).to_list()
    expected = graph.cypher(
        "MATCH (d:Doc) WITH d, text_bm25(d, 'text', 'quick') AS s WHERE s > 0 RETURN d.id AS id, s AS score "
        "ORDER BY score DESC"
    ).to_list()
    assert [r["id"] for r in rows] == [r["id"] for r in expected] == [4, 2, 1]
    assert [r["score"] for r in rows] == pytest.approx([r["score"] for r in expected])


@pytest.mark.parametrize("mode", EDGE_MODES)
def test_fulltext_options_limit_and_skip(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    query = "CALL db.index.fulltext.queryNodes('Doc.text', 'quick', {%s}) YIELD node RETURN node.id AS id"
    assert graph.cypher(query % "limit: 2").to_list() == [{"id": 4}, {"id": 2}]
    assert graph.cypher(query % "limit: 2, skip: 1").to_list() == [{"id": 2}, {"id": 1}]
    assert graph.cypher(query % "skip: 2").to_list() == [{"id": 1}]
    assert graph.cypher(query % "limit: 0").to_list() == []
    assert graph.cypher(query % "").to_list() == [{"id": 4}, {"id": 2}, {"id": 1}]
    assert (
        graph.cypher("CALL db.index.fulltext.queryNodes('Doc.text', 'nomatchterm') YIELD node RETURN node").to_list()
        == []
    )


def test_the_map_form_and_yield_columns_still_work(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    mapped = graph.cypher(
        "CALL db.index.fulltext.queryNodes({indexName: 'Doc.text', queryString: 'gamma'}) "
        "YIELD node, score WHERE score > 0 RETURN node.id AS id"
    ).to_list()
    assert mapped == [{"id": 3}]
    both = graph.cypher("CALL db.index.fulltext.queryNodes('Doc.text', 'gamma') YIELD node, score RETURN count(*) AS c")
    assert both.to_list() == [{"c": 1}]
    # an existing procedure keeps its map-only form, and says so
    with pytest.raises(kglite.CypherSyntaxError, match="map syntax"):
        graph.cypher("CALL db.labels(1)")
    with pytest.raises(kglite.CypherSyntaxError, match="Positional arguments are accepted only by db.index"):
        graph.cypher("CALL pagerank(0.85) YIELD node")


@pytest.mark.parametrize(
    ("call", "message"),
    [
        ("CALL db.index.fulltext.queryNodes('Doc.text') YIELD node", "takes 2 to 3 arguments but 1 argument was given"),
        ("CALL db.index.fulltext.queryNodes('a', 'b', {}, 4) YIELD node", "but 4 arguments were given"),
        ("CALL db.index.vector.queryNodes('Doc.text', 2) YIELD node", "takes 3 arguments but 2 arguments were given"),
    ],
)
def test_wrong_arity_is_refused_with_the_signature(call: str, message: str, tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherSyntaxError, match=message) as info:
        graph.cypher(call)
    assert "indexName :: STRING" in str(info.value)


@pytest.mark.parametrize(
    ("call", "params", "message"),
    [
        (
            "CALL db.index.fulltext.queryNodes($i, $q) YIELD node",
            {"i": 5, "q": "x"},
            "'indexName' must be a non-empty string",
        ),
        (
            "CALL db.index.fulltext.queryNodes($i, $q) YIELD node",
            {"i": "Doc.text", "q": 7},
            "'queryString' must be a string",
        ),
        (
            "CALL db.index.fulltext.queryNodes($i, $q) YIELD node",
            {"i": "Doc.text", "q": None},
            "'queryString' must be a string",
        ),
        ("CALL db.index.fulltext.queryNodes('Doc.text', 'q', 5) YIELD node", {}, "'options' must be a map"),
        (
            "CALL db.index.fulltext.queryNodes('Doc.text', 'q', {limit: -1}) YIELD node",
            {},
            "option 'limit' must be a non-negative integer",
        ),
        (
            "CALL db.index.fulltext.queryNodes('Doc.text', 'q', {limit: 'x'}) YIELD node",
            {},
            "option 'limit' must be a non-negative integer",
        ),
        (
            "CALL db.index.fulltext.queryNodes('Doc.text', 'q', {analyzer: 'x'}) YIELD node",
            {},
            "unknown parameter 'analyzer'",
        ),
        (
            "CALL db.index.vector.queryNodes('Doc.text', 'k', [1.0, 0.0]) YIELD node",
            {},
            "'numberOfNearestNeighbours' must be an integer",
        ),
        ("CALL db.index.vector.queryNodes('Doc.text', 0, [1.0, 0.0]) YIELD node", {}, "must be a positive integer"),
        ("CALL db.index.vector.queryNodes('Doc.text', 2, 'v') YIELD node", {}, "'query' must be a list of numbers"),
        ("CALL db.index.vector.queryNodes('Doc.text', 2, ['a']) YIELD node", {}, "'query' must be a list of numbers"),
        ("CALL db.index.vector.queryNodes('Doc.text', 2, [1.0, 0.0, 0.0]) YIELD node", {}, "dimension"),
    ],
)
def test_type_errors_name_the_argument(call: str, params: dict, message: str, tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=message):
        graph.cypher(call, params=params)


def test_unknown_index_names_what_is_listed(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=r"no FULLTEXT index named 'Nope.text'.*'Doc.text'"):
        graph.cypher("CALL db.index.fulltext.queryNodes('Nope.text', 'x') YIELD node")
    # the kind decides: a VECTOR row is not a FULLTEXT index and vice versa
    graph.drop_text_index("Doc", "text")
    with pytest.raises(kglite.CypherExecutionError, match=r"FULLTEXT indexes over nodes today: none exist"):
        graph.cypher("CALL db.index.fulltext.queryNodes('Doc.text', 'x') YIELD node")
    with pytest.raises(kglite.CypherExecutionError, match=r"no VECTOR index named 'Doc.text'.*\n?.*relationship"):
        graph.cypher("CALL db.index.vector.queryRelationships('Doc.text', 1, [1.0, 0.0]) YIELD relationship")


def test_yield_validation_and_signature_listing(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    with pytest.raises(kglite.CypherError, match="relationship"):
        graph.cypher("CALL db.index.fulltext.queryNodes('Doc.text', 'x') YIELD relationship")
    rows = graph.cypher(
        "SHOW PROCEDURES YIELD name, signature, mode WHERE name STARTS WITH 'db.index.' RETURN name, signature, mode "
        "ORDER BY name"
    ).to_list()
    assert [r["name"] for r in rows] == [
        "db.index.fulltext.queryNodes",
        "db.index.fulltext.queryRelationships",
        "db.index.vector.queryNodes",
        "db.index.vector.queryRelationships",
    ]
    assert rows[0]["signature"] == (
        "db.index.fulltext.queryNodes(indexName :: STRING, queryString :: STRING, options = {} :: MAP) "
        ":: (node :: ANY?, score :: ANY?)"
    )
    assert rows[2]["signature"].startswith(
        "db.index.vector.queryNodes(indexName :: STRING, numberOfNearestNeighbours :: INTEGER, query :: LIST<FLOAT>)"
    )
    assert {r["mode"] for r in rows} == {"READ"}


# ── vector ────────────────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_vector_positional_equals_node_embeddings_query(mode: str, tmp_path: Path) -> None:
    graph = _graph(mode, tmp_path)
    rows = graph.cypher(
        "CALL db.index.vector.queryNodes($index, $k, $vector) YIELD node AS entity, score "
        "RETURN entity.id AS id, score ORDER BY score DESC LIMIT $k",
        params={"index": "Doc.text", "k": 3, "vector": [1.0, 0.1]},
    ).to_list()
    expected = graph.cypher(
        "CALL db.node_embeddings.query({type: 'Doc', text_column: 'text', vector: [1.0, 0.1], top_k: 3}) "
        "YIELD node, score RETURN node.id AS id, score ORDER BY score DESC"
    ).to_list()
    assert [r["id"] for r in rows] == [r["id"] for r in expected] == [1, 4, 3]
    assert [r["score"] for r in rows] == pytest.approx([r["score"] for r in expected])


# ── relationships ─────────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", EDGE_MODES)
def test_relationship_fulltext_and_vector(mode: str) -> None:
    graph = _rel_graph(mode)
    ft = graph.cypher(
        "CALL db.index.fulltext.queryRelationships('relationship:CLAIMS.text', 'quick', {limit: 5}) "
        "YIELD relationship AS entity, score RETURN entity.k AS k, type(entity) AS t, score ORDER BY score DESC"
    ).to_list()
    assert sorted(r["k"] for r in ft) == [0, 1]
    assert {r["t"] for r in ft} == {"CLAIMS"}
    expected = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WITH r, text_bm25(r, 'text', 'quick') AS s WHERE s > 0 "
        "RETURN r.k AS k, s AS score ORDER BY score DESC"
    ).to_list()
    assert [r["k"] for r in ft] == [r["k"] for r in expected]
    assert [r["score"] for r in ft] == pytest.approx([r["score"] for r in expected])
    vec = graph.cypher(
        "CALL db.index.vector.queryRelationships('relationship:CLAIMS.text', 2, [1.0, 0.1]) "
        "YIELD relationship, score RETURN relationship.k AS k, score ORDER BY score DESC"
    ).to_list()
    assert [r["k"] for r in vec] == [0, 2]
    with pytest.raises(kglite.CypherExecutionError, match="no FULLTEXT index named 'Doc.text'"):
        graph.cypher("CALL db.index.fulltext.queryRelationships('Doc.text', 'q') YIELD relationship")


# ── reads only, and fresh ─────────────────────────────────────────────────


def test_the_procedures_are_reads_and_see_new_nodes(tmp_path: Path) -> None:
    graph = _graph("memory", tmp_path)
    before = graph.cypher("MATCH (n) RETURN count(n) AS c").to_list()
    graph.cypher("CALL db.index.fulltext.queryNodes('Doc.text', 'quick') YIELD node RETURN count(node) AS c")
    assert graph.cypher("MATCH (n) RETURN count(n) AS c").to_list() == before
    graph.cypher("CREATE (:Doc {id: 5, text: 'quick quick quick quick quick'})")
    rows = graph.cypher(
        "CALL db.index.fulltext.queryNodes('Doc.text', 'quick') YIELD node RETURN node.id AS id"
    ).to_list()
    assert rows[0] == {"id": 5}
