"""`DROP INDEX` against the names `SHOW INDEXES` prints.

A relationship vector index is listed as ``relationship:CLAIMS.text``. That
name has to make the round trip — printed by one statement, pasted into the
next — in every spelling a client might use: bare, backticked, and with
``IF EXISTS``. The node vector index is the control: its unprefixed name must
keep reaching the node structures and nothing else.
"""

from __future__ import annotations

import pytest

from kglite import KnowledgeGraph

VECTORS = [[1.0, 0.0], [0.8, 0.6], [0.0, 1.0]]


def _graph() -> KnowledgeGraph:
    """Three ``Doc`` nodes, two indexed ``CLAIMS`` edges, one indexed node store."""
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 0, text: 'hub'})")
    for index, vector in enumerate(VECTORS[:2], start=1):
        graph.cypher(
            "MATCH (hub:Doc {id: 0}) "
            "CREATE (hub)-[:CLAIMS {rank: $rank, text: $text}]->(:Doc {id: $rank, text: $text})",
            params={"rank": index, "text": f"body {index}"},
        )
        graph.cypher(
            "MATCH ()-[r:CLAIMS {rank: $rank}]->() "
            "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
            "entries:[{relationship:r, vector:$vector}]}) YIELD stored RETURN stored",
            params={"rank": index, "vector": vector},
        )
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )
    graph.set_embeddings("Doc", "text", {0: VECTORS[0], 1: VECTORS[1], 2: VECTORS[2]})
    graph.build_vector_index("Doc", "text")
    return graph


def _index_names(graph: KnowledgeGraph) -> list[str]:
    return [row["name"] for row in graph.cypher("SHOW INDEXES").to_list()]


def _edge_index_state(graph: KnowledgeGraph) -> str:
    rows = graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS'}) YIELD index_state RETURN index_state"
    ).to_list()
    assert len(rows) == 1, rows
    return rows[0]["index_state"]


@pytest.mark.parametrize(
    "statement",
    [
        "DROP INDEX relationship:CLAIMS.text",
        "DROP INDEX `relationship:CLAIMS.text`",
        "DROP INDEX relationship:CLAIMS.text IF EXISTS",
        "DROP INDEX `relationship:CLAIMS.text` IF EXISTS",
    ],
)
def test_relationship_index_drops_under_every_spelling_of_its_printed_name(statement: str) -> None:
    graph = _graph()
    assert sorted(_index_names(graph)) == ["Doc.text", "relationship:CLAIMS.text"]
    assert _edge_index_state(graph) == "online"

    graph.cypher(statement)

    assert _index_names(graph) == ["Doc.text"]
    assert _edge_index_state(graph) == "none"
    # The accelerator goes; the vectors are data and stay.
    stores = graph.cypher("CALL db.relationship_embeddings.list({type:'CLAIMS'}) YIELD count RETURN count").to_list()
    assert stores == [{"count": 2}]


def test_node_and_relationship_indexes_drop_independently() -> None:
    graph = _graph()

    graph.cypher("DROP INDEX Doc.text")
    assert _index_names(graph) == ["relationship:CLAIMS.text"]
    assert _edge_index_state(graph) == "online"

    graph.cypher("DROP INDEX relationship:CLAIMS.text")
    assert _index_names(graph) == []


def test_if_exists_is_a_no_op_only_for_a_name_nothing_carries() -> None:
    graph = _graph()

    graph.cypher("DROP INDEX relationship:CLAIMS.missing IF EXISTS")
    assert _edge_index_state(graph) == "online"

    with pytest.raises(Exception, match="relationship:CLAIMS.missing"):
        graph.cypher("DROP INDEX relationship:CLAIMS.missing")
    assert _edge_index_state(graph) == "online"
