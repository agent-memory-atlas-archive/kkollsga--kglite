"""A text index refreshed inside a write statement that then fails.

`text_bm25` catches its index up at query entry, so inside a write statement
it can fold in text the statement itself wrote. When a later clause fails, the
rollback restores the property (or removes the created node); the index must
not keep the rolled-back words, and `SHOW INDEXES` must say what is true.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite

MODES = ["memory", "mapped"]

SCORES = "MATCH (d:Doc) RETURN d.doc_id AS id, text_bm25(d, 'body', $q) AS score ORDER BY id"

FAILING_TAIL = " WITH n, text_bm25(n, 'body', 'zebra') AS s RETURN s, 1 / 0 AS boom"


def _graph(mode: str) -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph(storage="mapped") if mode == "mapped" else kglite.KnowledgeGraph()
    graph.add_nodes(
        pd.DataFrame(
            {
                "doc_id": [1, 2, 3],
                "name": ["a", "b", "c"],
                "body": ["apple pie", "a quick brown fox", "slow green turtles"],
            }
        ),
        "Doc",
        "doc_id",
        "name",
    )
    graph.build_text_index("Doc", "body")
    return graph


def _scores(graph: kglite.KnowledgeGraph, query: str) -> list[dict]:
    return graph.cypher(SCORES, params={"q": query}).to_list()


def _text_row(graph: kglite.KnowledgeGraph) -> dict:
    rows = [row for row in graph.cypher("SHOW INDEXES").to_list() if row["type"] == "FULLTEXT"]
    assert len(rows) == 1, rows
    return rows[0]


def _fail(graph: kglite.KnowledgeGraph, write: str) -> None:
    with pytest.raises(Exception):
        graph.cypher(write + FAILING_TAIL)


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize(
    "write",
    [
        "MATCH (n:Doc {doc_id: 1}) SET n.body = 'zebra stripes'",
        "MATCH (n:Doc {doc_id: 1}) SET n.body = 'zebra' REMOVE n.body",
        "CREATE (n:Doc {doc_id: 9, name: 'z', body: 'zebra zebra zebra'})",
    ],
    ids=["set", "set-then-remove", "create"],
)
def test_failed_statement_leaves_pre_statement_scores(mode, write) -> None:
    graph = _graph(mode)
    before = {query: _scores(graph, query) for query in ("zebra", "apple", "quick fox")}

    _fail(graph, write)

    assert graph.cypher("MATCH (d:Doc) RETURN count(d) AS n").to_list() == [{"n": 3}]
    # Honest before the next read catches it up: the refresh the failed
    # statement did has to be re-done, so the index reports it.
    row = _text_row(graph)
    assert row["state"] == "ONLINE"
    assert row["stale"] is True
    for query, expected in before.items():
        assert _scores(graph, query) == expected, query
    assert _text_row(graph)["stale"] is False


@pytest.mark.parametrize("mode", MODES)
def test_committed_statement_keeps_its_refresh(mode) -> None:
    graph = _graph(mode)
    graph.cypher(
        "MATCH (n:Doc {doc_id: 1}) SET n.body = 'zebra stripes' WITH n, text_bm25(n, 'body', 'zebra') AS s RETURN s"
    )
    assert _text_row(graph)["stale"] is False
    scores = {row["id"]: row["score"] for row in _scores(graph, "zebra")}
    assert scores[1] > 0.0
    assert scores[2] == 0.0


def test_disk_mode_refuses_a_text_index(tmp_path) -> None:
    graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "graph"))
    graph.cypher("CREATE (:Doc {doc_id: 1, body: 'apple pie'})")
    with pytest.raises(ValueError, match="disk-backed graph"):
        graph.build_text_index("Doc", "body")
