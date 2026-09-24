"""Relationship BM25 text indexes: `db.relationship_text_index.*` and `text_bm25(r, …)`.

Every freshness assertion compares against a rebuilt index on a copy of the
same graph — BM25 scores move with the corpus, so "the same corpus scores the
same" is the only stable claim.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ["memory", "mapped"]

BUILD = "CALL db.relationship_text_index.build({type: 'CLAIMS', property: 'text'}) YIELD indexed RETURN indexed"
SCORES = "MATCH ()-[r:CLAIMS]->() RETURN r.k AS k, text_bm25(r, 'text', $q) AS s ORDER BY k"


def _graph(mode: str) -> KnowledgeGraph:
    graph = KnowledgeGraph(storage="mapped") if mode == "mapped" else KnowledgeGraph()
    graph.cypher(
        "CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), "
        "(a)-[:CLAIMS {k: 0, text: 'the quick brown fox'}]->(b), "
        "(a)-[:CLAIMS {k: 1, text: 'a quick brown marmoset appears'}]->(b), "
        "(a)-[:CLAIMS {k: 2, text: 'slow green turtles'}]->(b)"
    )
    assert graph.cypher(BUILD).to_list() == [{"indexed": 3}]
    return graph


def _scores(graph: KnowledgeGraph, query: str) -> list[dict]:
    return graph.cypher(SCORES, params={"q": query}).to_list()


def _rebuilt(graph: KnowledgeGraph, query: str) -> list[dict]:
    copy = graph.copy()
    copy.cypher(BUILD)
    return _scores(copy, query)


def _row(graph: KnowledgeGraph) -> dict:
    rows = [row for row in graph.cypher("SHOW INDEXES").to_list() if row["entityType"] == "RELATIONSHIP"]
    assert len(rows) == 1, rows
    return rows[0]


def _stale(graph: KnowledgeGraph) -> bool:
    return graph.cypher("CALL db.relationship_text_index.list() YIELD index_state RETURN index_state").to_list() == [
        {"index_state": "stale"}
    ]


@pytest.mark.parametrize("mode", MODES)
def test_lifecycle_build_query_refresh_drop(mode) -> None:
    graph = _graph(mode)
    scores = {row["k"]: row["s"] for row in _scores(graph, "marmoset")}
    assert scores[0] == 0.0 and scores[2] == 0.0
    assert scores[1] > 0.0
    graph.cypher("MATCH ()-[r:CLAIMS {k: 2}]->() SET r.text = 'marmoset marmoset'")
    assert _stale(graph)
    assert graph.cypher(
        "CALL db.relationship_text_index.refresh({type: 'CLAIMS', property: 'text'}) YIELD refreshed RETURN refreshed"
    ).to_list() == [{"refreshed": 1}]
    assert not _stale(graph)
    assert _scores(graph, "marmoset") == _rebuilt(graph, "marmoset")
    assert graph.cypher(
        "CALL db.relationship_text_index.drop({type: 'CLAIMS', property: 'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": True}]
    with pytest.raises(Exception, match="db.relationship_text_index.build"):
        _scores(graph, "marmoset")


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize(
    "write",
    [
        "MATCH ()-[r:CLAIMS {k: 2}]->() SET r.text = 'zebra quick'",
        "MATCH ()-[r:CLAIMS {k: 0}]->() REMOVE r.text",
        "MATCH ()-[r:CLAIMS {k: 1}]->() DELETE r",
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {k: 7, text: 'zebra member'}]->(b)",
        "MATCH ()-[r:CLAIMS {k: 1}]->() DELETE r WITH 1 AS x "
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {k: 8, text: 'zebra reuse'}]->(b)",
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) MERGE (a)-[:CLAIMS {k: 9, text: 'zebra merged'}]->(b)",
    ],
    ids=["set", "remove", "delete", "create-parallel", "delete-then-create", "merge"],
)
def test_freshness_after_each_cypher_write(mode, write) -> None:
    graph = _graph(mode)
    graph.cypher(write)
    for query in ("zebra", "quick", "marmoset"):
        assert _scores(graph, query) == _rebuilt(graph, query), query


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("conflict", ["update", "replace", "sum", "preserve"])
def test_freshness_after_add_connections(mode, conflict) -> None:
    graph = KnowledgeGraph(storage="mapped") if mode == "mapped" else KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"id": [1, 2, 3], "name": ["a", "b", "c"]}), "Doc", "id", "name")
    # (1 -> 2) starts without text, so even `preserve` changes its document.
    graph.add_connections(
        pd.DataFrame({"src": [1, 2], "dst": [2, 3], "text": [None, "base words"]}),
        "CLAIMS",
        "Doc",
        "src",
        "Doc",
        "dst",
    )
    graph.cypher(
        "CALL db.relationship_text_index.build({type: 'CLAIMS', property: 'text'}) YIELD indexed RETURN indexed"
    )
    graph.add_connections(
        pd.DataFrame({"src": [1, 1], "dst": [2, 3], "text": ["zebra words", "zebra again"]}),
        "CLAIMS",
        "Doc",
        "src",
        "Doc",
        "dst",
        conflict_handling=conflict,
    )
    query = (
        "MATCH (a)-[r:CLAIMS]->(b) RETURN a.id AS a, b.id AS b, r.text AS t, "
        "text_bm25(r, 'text', 'zebra') AS s ORDER BY a, b"
    )
    copy = graph.copy()
    copy.cypher(
        "CALL db.relationship_text_index.build({type: 'CLAIMS', property: 'text'}) YIELD indexed RETURN indexed"
    )
    assert graph.cypher(query).to_list() == copy.cypher(query).to_list()


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize(
    "write",
    [
        "MATCH ()-[r:CLAIMS {k: 0}]->() SET r.text = 'zebra stripes'",
        "MATCH ()-[r:CLAIMS {k: 1}]->() DELETE r",
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {k: 7, text: 'zebra zebra'}]->(b)",
    ],
    ids=["set", "delete", "create"],
)
def test_failed_statement_leaves_pre_statement_scores(mode, write) -> None:
    graph = _graph(mode)
    before = {query: _scores(graph, query) for query in ("zebra", "marmoset")}
    with pytest.raises(Exception):
        graph.cypher(
            write + " WITH 1 AS x MATCH ()-[r:CLAIMS]->() "
            "WITH r, text_bm25(r, 'text', 'zebra') AS s RETURN s, 1 / 0 AS boom"
        )
    for query, expected in before.items():
        assert _scores(graph, query) == expected, query


@pytest.mark.parametrize("mode", MODES)
def test_save_and_load_keep_the_index(mode, tmp_path) -> None:
    graph = _graph(mode)
    graph.cypher("MATCH ()-[r:CLAIMS {k: 2}]->() SET r.text = 'zebra pending'")
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    loaded = kglite.load(path, storage=mode) if mode == "mapped" else kglite.load(path)
    assert _row(loaded)["name"] == "relationship:CLAIMS.text"
    assert _row(loaded)["stale"] is True, "the pending SET travels with the index"
    assert _scores(loaded, "zebra") == _rebuilt(graph, "zebra")


def test_save_without_an_index_writes_no_section(tmp_path) -> None:
    graph = _graph("memory")
    graph.cypher(
        "CALL db.relationship_text_index.drop({type: 'CLAIMS', property: 'text'}) YIELD dropped RETURN dropped"
    )
    path = tmp_path / "g.kgl"
    graph.save(str(path))
    assert b"edge_text_index" not in path.read_bytes()


@pytest.mark.parametrize("mode", MODES)
def test_show_indexes_and_drop_index(mode) -> None:
    graph = _graph(mode)
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() CALL db.relationship_embeddings.set({type: 'CLAIMS', text_property: 'text', "
        "entries: [{relationship: r, vector: [1.0, 0.0]}]}) YIELD stored RETURN count(*)"
    )
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type: 'CLAIMS', text_property: 'text'}) YIELD indexed RETURN "
        "indexed"
    )
    rows = graph.cypher("SHOW INDEXES").to_list()
    kinds = sorted(row["type"] for row in rows if row["name"] == "relationship:CLAIMS.text")
    assert kinds == ["FULLTEXT", "VECTOR"]
    text_row = next(row for row in rows if row["type"] == "FULLTEXT")
    assert text_row["entityType"] == "RELATIONSHIP"
    assert text_row["stale"] is False
    graph.cypher("DROP INDEX relationship:CLAIMS.text")
    assert graph.cypher("SHOW INDEXES").to_list() == []
    # The vectors stay; only the two accelerators went.
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type: 'CLAIMS'}) YIELD count, index_state RETURN count, index_state"
    ).to_list() == [{"count": 3, "index_state": "none"}]


@pytest.mark.parametrize("mode", MODES)
def test_text_bm25_on_edge_embeddings_query_values(mode) -> None:
    graph = _graph(mode)
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() CALL db.relationship_embeddings.set({type: 'CLAIMS', text_property: 'text', "
        "entries: [{relationship: r, vector: [toFloat(r.k), 1.0]}]}) YIELD stored RETURN count(*)"
    )
    rows = graph.cypher(
        "CALL db.relationship_embeddings.query({type: 'CLAIMS', text_property: 'text', vector: [0.0, 1.0], "
        "top_k: 3, exact: true}) YIELD relationship "
        "RETURN relationship.k AS k, text_bm25(relationship, 'text', 'marmoset') AS s ORDER BY k"
    ).to_list()
    assert rows == _scores(graph, "marmoset")


@pytest.mark.parametrize("mode", MODES)
def test_hybrid_score_fuse_over_relationships(mode) -> None:
    graph = _graph(mode)
    graph.cypher(
        "MATCH ()-[r:CLAIMS]->() CALL db.relationship_embeddings.set({type: 'CLAIMS', text_property: 'text', "
        "entries: [{relationship: r, vector: [toFloat(r.k), 1.0]}]}) YIELD stored RETURN count(*)"
    )
    rows = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() RETURN r.k AS k, text_bm25(r, 'text', 'marmoset') AS t, "
        "vector_score(r, 'text_emb', [1.0, 0.0]) AS v, "
        "score_fuse(text_bm25(r, 'text', 'marmoset'), vector_score(r, 'text_emb', [1.0, 0.0])) AS f "
        "ORDER BY k"
    ).to_list()
    for row in rows:
        assert row["f"] == pytest.approx((row["t"] + row["v"]) / 2)
    top = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() RETURN r.k AS k, "
        "score_fuse(text_bm25(r, 'text', 'marmoset'), vector_score(r, 'text_emb', [1.0, 0.0])) AS f "
        "ORDER BY f DESC LIMIT 1"
    ).to_list()
    assert top == [{"k": max(rows, key=lambda row: row["f"])["k"], "f": max(row["f"] for row in rows)}]


def test_disk_mode_refuses_like_the_node_index(tmp_path) -> None:
    graph = KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    graph.cypher("CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), (a)-[:CLAIMS {text: 'quick'}]->(b)")
    with pytest.raises(Exception, match="not supported on a disk-backed graph"):
        graph.cypher(BUILD)
    with pytest.raises(ValueError, match="not supported on a disk-backed graph"):
        graph.build_text_index("Doc", "id")
