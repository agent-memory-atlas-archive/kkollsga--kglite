"""Vector-index refresh honesty and the delete contract, nodes and relationships.

A refresh with no index to refresh used to answer ``0`` / ``{refreshed: 0}``,
which reads as "nothing outstanding" — exactly what an agent saw after a delete
had dropped the index. Both entities now refuse, naming the store and the build
call. The delete matrix below pins which mutations drop an HNSW index (the
guide and ``describe()`` state the same matrix), per storage mode.
"""

from __future__ import annotations

import math
from pathlib import Path
from typing import Callable

import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ["memory", "mapped", "disk"]
EDGE_REFRESH = (
    "CALL db.edge_embeddings.refresh_index({type:'CLAIMS', text_property:'text'}) YIELD refreshed RETURN refreshed"
)
EDGE_BUILD_CALL = "db.edge_embeddings.build_index({type: 'CLAIMS', text_property: 'text'})"


def _empty(mode: str, tmp_path: Path) -> KnowledgeGraph:
    if mode == "disk":
        return kglite.open(str(tmp_path / "disk-graph"), storage="disk")
    return KnowledgeGraph(storage=mode)


def _graph(mode: str, tmp_path: Path, count: int = 8, *, indexed: bool = True) -> KnowledgeGraph:
    """`count` embedded Doc nodes, each the target of one embedded CLAIMS edge from a Hub."""
    graph = _empty(mode, tmp_path)
    graph.cypher("CREATE (:Hub {id: 100})")
    vectors = {i: [math.cos(2 * math.pi * i / count), math.sin(2 * math.pi * i / count)] for i in range(count)}
    for i in range(count):
        graph.cypher(
            "MATCH (h:Hub {id: 100}) CREATE (h)-[:CLAIMS {rank: $i, text: 't'}]->(:Doc {id: $i, summary: 's'})",
            params={"i": i},
        )
        graph.cypher(
            "MATCH ()-[r:CLAIMS {rank: $i}]->() "
            "CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:$v}]}) YIELD stored RETURN stored",
            params={"i": i, "v": vectors[i]},
        )
    graph.set_embeddings("Doc", "summary", vectors)
    if indexed:
        graph.build_vector_index("Doc", "summary")
        graph.cypher(
            "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'}) YIELD indexed RETURN indexed"
        )
    return graph


def _edge_state(graph: KnowledgeGraph) -> str:
    rows = graph.cypher("CALL db.edge_embeddings.list({type:'CLAIMS'}) YIELD index_state RETURN index_state").to_list()
    return rows[0]["index_state"]


def _state(graph: KnowledgeGraph) -> tuple[bool, str]:
    return graph.has_vector_index("Doc", "summary"), _edge_state(graph)


# ── refusals ─────────────────────────────────────────────────────────────────


class TestNodeRefresh:
    def test_a_store_with_no_index_refuses_naming_the_build_call(self, tmp_path: Path) -> None:
        graph = _graph("memory", tmp_path, indexed=False)
        with pytest.raises(ValueError, match=r"no vector index on 'Doc\.summary_emb'") as info:
            graph.refresh_vector_index("Doc", "summary")
        assert "build_vector_index('Doc', 'summary')" in str(info.value)

    def test_a_missing_store_refuses_naming_the_store(self, tmp_path: Path) -> None:
        graph = _graph("memory", tmp_path)
        with pytest.raises(ValueError, match=r"no embedding store 'Doc\.nope_emb'"):
            graph.refresh_vector_index("Doc", "nope")

    def test_an_index_a_delete_dropped_refuses_instead_of_answering_zero(self, tmp_path: Path) -> None:
        graph = _graph("memory", tmp_path)
        assert graph.refresh_vector_index("Doc", "summary") == 0, "a current index still answers 0"
        graph.cypher("MATCH (n:Doc {id: 1}) DETACH DELETE n")
        assert graph.has_vector_index("Doc", "summary") is False
        with pytest.raises(ValueError, match=r"build_vector_index\('Doc', 'summary'\)"):
            graph.refresh_vector_index("Doc", "summary")
        graph.build_vector_index("Doc", "summary")
        assert graph.refresh_vector_index("Doc", "summary") == 0


class TestRelationshipRefresh:
    def test_a_store_with_no_index_refuses_naming_the_build_call(self, tmp_path: Path) -> None:
        graph = _graph("memory", tmp_path, indexed=False)
        with pytest.raises(
            kglite.CypherExecutionError, match=r"no vector index on relationship store 'CLAIMS\.text_emb'"
        ) as info:
            graph.cypher(EDGE_REFRESH)
        assert EDGE_BUILD_CALL in str(info.value)

    def test_an_index_a_delete_dropped_refuses_instead_of_answering_zero(self, tmp_path: Path) -> None:
        graph = _graph("memory", tmp_path)
        assert graph.cypher(EDGE_REFRESH).to_list() == [{"refreshed": 0}]
        graph.cypher("MATCH ()-[r:CLAIMS {rank: 1}]->() DELETE r")
        assert _edge_state(graph) == "none"
        with pytest.raises(kglite.CypherExecutionError) as info:
            graph.cypher(EDGE_REFRESH)
        assert EDGE_BUILD_CALL in str(info.value)
        graph.cypher(
            "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'}) YIELD indexed RETURN indexed"
        )
        assert graph.cypher(EDGE_REFRESH).to_list() == [{"refreshed": 0}]


# ── the delete matrix ────────────────────────────────────────────────────────


def _failing(statement: str) -> Callable[[KnowledgeGraph], None]:
    """Run `statement` inside one that fails after it, so the delete is undone."""

    def run(graph: KnowledgeGraph) -> None:
        with pytest.raises(kglite.CypherExecutionError, match="division by zero"):
            graph.cypher(f"{statement} WITH count(*) AS c RETURN 1 / (c - c) AS boom")

    return run


def _rolled_back_transaction(graph: KnowledgeGraph) -> None:
    tx = graph.begin()
    tx.cypher("MATCH (n:Doc {id: 1}) DETACH DELETE n")
    tx.rollback()


def _committed_transaction(graph: KnowledgeGraph) -> None:
    tx = graph.begin()
    tx.cypher("MATCH ()-[r:CLAIMS {rank: 1}]->() DELETE r")
    tx.commit()


def _cypher(statement: str) -> Callable[[KnowledgeGraph], None]:
    return lambda graph: graph.cypher(statement)


# (mutation, node index survives, relationship index survives) — identical in
# every storage mode.
DELETE_MATRIX: list[tuple[str, Callable[[KnowledgeGraph], None], bool, bool]] = [
    ("SET relationship text", _cypher("MATCH ()-[r:CLAIMS {rank: 1}]->() SET r.text = 'u'"), True, True),
    ("SET node text", _cypher("MATCH (n:Doc {id: 1}) SET n.summary = 'u'"), True, True),
    ("CREATE relationship", _cypher("MATCH (h:Hub), (d:Doc {id: 2}) CREATE (h)-[:CLAIMS {rank: 99}]->(d)"), True, True),
    ("CREATE node", _cypher("CREATE (:Doc {id: 99, summary: 'x'})"), True, True),
    ("DELETE embedded relationship", _cypher("MATCH ()-[r:CLAIMS {rank: 1}]->() DELETE r"), True, False),
    ("DETACH DELETE embedded endpoint", _cypher("MATCH (n:Doc {id: 1}) DETACH DELETE n"), False, False),
    ("DETACH DELETE unembedded endpoint", _cypher("MATCH (n:Hub) DETACH DELETE n"), True, False),
    (
        "DELETE unembedded relationship of the type",
        lambda g: (
            g.cypher("MATCH (h:Hub), (d:Doc {id: 2}) CREATE (h)-[:CLAIMS {rank: 99}]->(d)"),
            g.cypher("MATCH ()-[r:CLAIMS {rank: 99}]->() DELETE r"),
        ),
        True,
        True,
    ),
    ("failed statement undoes DELETE r", _failing("MATCH ()-[r:CLAIMS {rank: 1}]->() DELETE r"), True, True),
    ("failed statement undoes DETACH DELETE", _failing("MATCH (n:Doc {id: 1}) DETACH DELETE n"), True, True),
    ("rolled-back transaction", _rolled_back_transaction, True, True),
    ("committed transaction DELETE r", _committed_transaction, True, False),
    ("vacuum() with nothing deleted", lambda g: g.vacuum(), True, True),
]


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize(
    ("mutation", "node_kept", "edge_kept"),
    [pytest.param(fn, node, edge, id=name) for name, fn, node, edge in DELETE_MATRIX],
)
def test_delete_matrix(
    mode: str, mutation: Callable[[KnowledgeGraph], None], node_kept: bool, edge_kept: bool, tmp_path: Path
) -> None:
    graph = _graph(mode, tmp_path)
    assert _state(graph) == (True, "online")
    mutation(graph)
    assert _state(graph) == (node_kept, "online" if edge_kept else "none")


@pytest.mark.parametrize(("mode", "node_kept"), [("memory", False), ("mapped", False), ("disk", True)])
def test_vacuum_after_a_delete_drops_every_index_except_on_disk(mode: str, node_kept: bool, tmp_path: Path) -> None:
    """A vacuum that compacts renumbers slots and drops node and relationship
    indexes alike; on disk `vacuum()` is a no-op, so only the delete's own drop
    stands."""
    graph = _graph(mode, tmp_path)
    graph.cypher("MATCH ()-[r:CLAIMS {rank: 1}]->() DELETE r")
    assert _state(graph) == (True, "none")
    graph.build_vector_index("Doc", "summary")
    graph.cypher(
        "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'}) YIELD indexed RETURN indexed"
    )
    assert _state(graph) == (True, "online")
    graph.vacuum()
    assert _state(graph) == (node_kept, "online" if mode == "disk" else "none")


def test_a_reopened_disk_graph_keeps_no_index(tmp_path: Path) -> None:
    graph = _graph("disk", tmp_path)
    graph.save(str(tmp_path / "disk-graph"))
    del graph
    reopened = kglite.open(str(tmp_path / "disk-graph"), storage="disk")
    assert _state(reopened) == (False, "none")
    with pytest.raises(ValueError, match="build_vector_index"):
        reopened.refresh_vector_index("Doc", "summary")
    with pytest.raises(kglite.CypherExecutionError, match="db.edge_embeddings.build_index"):
        reopened.cypher(EDGE_REFRESH)
