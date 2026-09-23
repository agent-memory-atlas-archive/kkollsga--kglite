"""Acceptance coverage for explicit relationship-vector index procedures."""

from __future__ import annotations

import math

import pytest

import kglite
from kglite import KnowledgeGraph


def _indexed_graph(count: int = 24) -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (:Hub {id: 0})")
    for index in range(count):
        angle = 2.0 * math.pi * index / count
        graph.cypher(
            "MATCH (hub:Hub {id: 0}) CREATE (hub)-[:CLAIMS {rank: $rank}]->(:Doc {id: $rank})",
            params={"rank": index},
        )
        graph.cypher(
            "MATCH ()-[r:CLAIMS {rank: $rank}]->() "
            "CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'text', "
            "entries:[{relationship:r, vector:$vector}]}) YIELD stored RETURN stored",
            params={"rank": index, "vector": [math.cos(angle), math.sin(angle)]},
        )
    return graph


def _query(graph: KnowledgeGraph, **options: object) -> list[dict]:
    vector = options.pop("vector", [1.0, 0.0])
    fields = ["type:'CLAIMS'", "text_property:'text'", "vector:$vector"]
    fields.extend(f"{name}:${name}" for name in options)
    return graph.cypher(
        f"CALL db.edge_embeddings.query({{{', '.join(fields)}}}) "
        "YIELD relationship, score, search_method "
        "RETURN relationship, score, search_method",
        params={"vector": vector, **options},
    ).to_list()


def test_exact_query_returns_typed_relationships_in_score_order() -> None:
    graph = _indexed_graph()
    rows = _query(graph, exact=True, top_k=3)

    assert [row["relationship"]["type"] for row in rows] == ["CLAIMS"] * 3
    ranks = [row["relationship"]["properties"]["rank"] for row in rows]
    assert ranks[0] == 0
    assert set(ranks[1:]) == {1, 23}
    assert all(set(row["relationship"]) == {"id", "start", "end", "type", "properties"} for row in rows)
    assert all(row["search_method"] == "exact" for row in rows)
    assert rows[0]["score"] == pytest.approx(1.0)


def test_index_lifecycle_and_hnsw_route_are_reported() -> None:
    graph = _indexed_graph()
    assert _query(graph, top_k=1)[0]["search_method"] == "exact"

    built = graph.cypher(
        "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text', "
        "m:8, ef_construction:64, ef_search:32}) YIELD indexed,metric,m RETURN indexed,metric,m"
    ).to_list()
    assert built == [{"indexed": 24, "metric": "cosine", "m": 8}]
    metadata = graph.cypher(
        "CALL db.edge_embeddings.list({type:'CLAIMS', text_property:'text'}) "
        "YIELD index_state,delta,unembedded RETURN index_state,delta,unembedded"
    ).to_list()
    assert metadata == [{"index_state": "online", "delta": 0, "unembedded": 0}]
    assert _query(graph, top_k=3)[0]["search_method"] == "hnsw"
    assert _query(graph, top_k=3, exact=True)[0]["search_method"] == "exact"

    assert graph.cypher(
        "CALL db.edge_embeddings.refresh_index({type:'CLAIMS', text_property:'text'}) YIELD refreshed RETURN refreshed"
    ).to_list() == [{"refreshed": 0}]
    assert graph.cypher(
        "CALL db.edge_embeddings.drop_index({type:'CLAIMS', text_property:'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": True}]
    assert _query(graph, top_k=1)[0]["search_method"] == "exact"


def test_query_where_filters_candidates_after_whole_store_top_k() -> None:
    graph = _indexed_graph()
    winner = _query(graph, exact=True, top_k=1)
    assert winner[0]["relationship"]["properties"]["rank"] == 0
    rows = graph.cypher(
        "CALL db.edge_embeddings.query({type:'CLAIMS', text_property:'text', "
        "vector:[1.0,0.0], top_k:1, exact:true}) "
        "YIELD relationship,score WHERE relationship.rank <> 0 "
        "RETURN relationship,score"
    ).to_list()
    assert rows == []

    constrained = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() WHERE r.rank <> 0 "
        "WITH r, vector_score(r,'text_emb',[1.0,0.0]) AS score "
        "RETURN r.rank AS rank,score ORDER BY score DESC LIMIT 1"
    ).to_list()
    assert constrained[0]["rank"] in {1, 23}


@pytest.mark.parametrize(
    ("extra", "message"),
    [
        ({"vector": [1.0]}, "dimension"),
        ({"vector": [float("nan"), 0.0]}, "finite|NaN"),
    ],
)
def test_invalid_query_options_fail_without_mutating_index(extra: dict, message: str) -> None:
    graph = _indexed_graph()
    graph.cypher(
        "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'}) YIELD indexed RETURN indexed"
    )
    with pytest.raises(kglite.CypherExecutionError, match=message):
        _query(graph, top_k=3, **extra)
    assert graph.cypher(
        "CALL db.edge_embeddings.list({type:'CLAIMS', text_property:'text'}) YIELD index_state RETURN index_state"
    ).to_list() == [{"index_state": "online"}]


def test_metric_without_hnsw_support_falls_back_to_exact() -> None:
    graph = _indexed_graph()
    graph.cypher(
        "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'text'}) YIELD indexed RETURN indexed"
    )
    rows = _query(graph, top_k=3, metric="poincare")
    assert rows
    assert all(row["search_method"] == "exact" for row in rows)
