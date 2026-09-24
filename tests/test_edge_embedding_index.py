"""Acceptance coverage for explicit relationship-vector index procedures."""

from __future__ import annotations

import json
import math
from pathlib import Path

import pytest

import kglite
from kglite import KnowledgeGraph


def _indexed_graph(count: int = 24, graph: KnowledgeGraph | None = None) -> KnowledgeGraph:
    graph = graph if graph is not None else KnowledgeGraph()
    graph.cypher("CREATE (:Hub {id: 0})")
    for index in range(count):
        angle = 2.0 * math.pi * index / count
        graph.cypher(
            "MATCH (hub:Hub {id: 0}) CREATE (hub)-[:CLAIMS {rank: $rank, text: 't'}]->(:Doc {id: $rank})",
            params={"rank": index},
        )
        graph.cypher(
            "MATCH ()-[r:CLAIMS {rank: $rank}]->() "
            "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
            "entries:[{relationship:r, vector:$vector}]}) YIELD stored RETURN stored",
            params={"rank": index, "vector": [math.cos(angle), math.sin(angle)]},
        )
    return graph


def _query(graph: KnowledgeGraph, **options: object) -> list[dict]:
    vector = options.pop("vector", [1.0, 0.0])
    fields = ["type:'CLAIMS'", "text_column:'text'", "vector:$vector"]
    fields.extend(f"{name}:${name}" for name in options)
    return graph.cypher(
        f"CALL db.relationship_embeddings.query({{{', '.join(fields)}}}) "
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
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text', "
        "m:8, ef_construction:64, ef_search:32}) YIELD indexed,metric,m RETURN indexed,metric,m"
    ).to_list()
    assert built == [{"indexed": 24, "metric": "cosine", "m": 8}]
    metadata = graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD index_state,delta,unembedded RETURN index_state,delta,unembedded"
    ).to_list()
    assert metadata == [{"index_state": "online", "delta": 0, "unembedded": 0}]
    assert _query(graph, top_k=3)[0]["search_method"] == "hnsw"
    assert _query(graph, top_k=3, exact=True)[0]["search_method"] == "exact"

    assert graph.cypher(
        "CALL db.relationship_embeddings.refresh_index({type:'CLAIMS', text_column:'text'}) YIELD refreshed RETURN "
        "refreshed"
    ).to_list() == [{"refreshed": 0}]
    assert graph.cypher(
        "CALL db.relationship_embeddings.drop_index({type:'CLAIMS', text_column:'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": True}]
    assert _query(graph, top_k=1)[0]["search_method"] == "exact"


def test_query_where_filters_candidates_after_whole_store_top_k() -> None:
    graph = _indexed_graph()
    winner = _query(graph, exact=True, top_k=1)
    assert winner[0]["relationship"]["properties"]["rank"] == 0
    rows = graph.cypher(
        "CALL db.relationship_embeddings.query({type:'CLAIMS', text_column:'text', "
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
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )
    with pytest.raises(kglite.CypherExecutionError, match=message):
        _query(graph, top_k=3, **extra)
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) YIELD index_state RETURN index_state"
    ).to_list() == [{"index_state": "online"}]


def test_metric_without_hnsw_support_falls_back_to_exact() -> None:
    """The fallback must return the metric's own ranking, not merely some rows.

    Asserting truthiness passed for any answer the exact scan produced,
    including one computed with the index's cosine metric; the rows below are
    the poincare distances of the 24-point unit circle against ``[1, 0]``: the
    aligned member at distance 0 and its two neighbours tied one step away.
    """
    graph = _indexed_graph()
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )
    rows = _query(graph, top_k=3, metric="poincare")

    assert [row["search_method"] for row in rows] == ["exact"] * 3
    ranks = [row["relationship"]["properties"]["rank"] for row in rows]
    assert ranks[0] == 0
    assert set(ranks[1:]) == {1, 23}
    assert rows[0]["score"] == pytest.approx(0.0, abs=1e-6)
    assert rows[1]["score"] == pytest.approx(-30.936417, rel=1e-4)
    assert rows[2]["score"] == pytest.approx(rows[1]["score"])
    assert rows[0]["score"] > rows[1]["score"]


def test_explicit_build_metric_is_recorded_and_serves_metric_less_queries() -> None:
    """An explicit build metric becomes the store's, so the default route uses it.

    Before the fix the store kept resolving cosine beside a euclidean index, so
    every ``query`` without ``metric`` mismatched the index and was served by
    exact scan while ``list`` reported cosine.
    """
    graph = _indexed_graph()
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) YIELD metric RETURN metric"
    ).to_list() == [{"metric": "cosine"}]

    built = graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text', "
        "metric:'euclidean'}) YIELD indexed,metric RETURN indexed,metric"
    ).to_list()
    assert built == [{"indexed": 24, "metric": "euclidean"}]
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) YIELD metric RETURN metric"
    ).to_list() == [{"metric": "euclidean"}]
    assert _query(graph, top_k=3)[0]["search_method"] == "hnsw"


def test_build_metric_contradicting_the_store_is_refused() -> None:
    graph = _indexed_graph()
    graph.cypher(
        "MATCH ()-[r:CLAIMS {rank: 0}]->() "
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
        "entries:[{relationship:r, vector:[1.0, 0.0]}], metric:'cosine'}) YIELD stored RETURN stored"
    )
    with pytest.raises(kglite.CypherExecutionError, match="declares metric 'cosine'"):
        graph.cypher(
            "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text', "
            "metric:'euclidean'}) YIELD indexed RETURN indexed"
        )
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD metric,index_state RETURN metric,index_state"
    ).to_list() == [{"metric": "cosine", "index_state": "none"}]


def _graph_in_mode(mode: str, tmp_path: Path) -> KnowledgeGraph:
    if mode == "disk":
        return kglite.open(str(tmp_path / "disk-graph"), storage="disk")
    return KnowledgeGraph(storage=mode)


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_query_route_serves_every_storage_mode(mode: str, tmp_path: Path) -> None:
    """The procedure route was only ever exercised on the in-memory backend.

    Mapped and disk keep edge properties in different substrates, and the query
    resolves a hit back to a public relationship through that substrate, so a
    backend that lost the mapping would answer with the wrong rows (or none)
    while every memory-mode test stayed green.
    """
    graph = _indexed_graph(6, _graph_in_mode(mode, tmp_path))

    exact = _query(graph, exact=True, top_k=2)
    assert [row["search_method"] for row in exact] == ["exact", "exact"]
    assert [row["relationship"]["type"] for row in exact] == ["CLAIMS", "CLAIMS"]
    assert exact[0]["relationship"]["properties"]["rank"] == 0
    assert exact[0]["score"] == pytest.approx(1.0)
    assert exact[1]["score"] == pytest.approx(0.5)
    assert exact[1]["relationship"]["properties"]["rank"] in {1, 5}

    assert graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    ).to_list() == [{"indexed": 6}]
    approximate = _query(graph, top_k=2)
    assert [row["search_method"] for row in approximate] == ["hnsw", "hnsw"]
    assert approximate[0]["relationship"]["properties"]["rank"] == 0
    assert approximate[0]["score"] == pytest.approx(1.0)
    assert approximate[1]["score"] == pytest.approx(0.5)


def test_drop_store_removes_every_vector_and_is_idempotent() -> None:
    graph = _indexed_graph(3)
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )

    assert graph.cypher(
        "CALL db.relationship_embeddings.drop({type:'CLAIMS', text_column:'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": True}]
    assert graph.cypher(
        "CALL db.relationship_embeddings.drop({type:'CLAIMS', text_column:'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": False}]
    assert graph.cypher("CALL db.relationship_embeddings.list() YIELD entity RETURN entity").to_list() == []
    with pytest.raises(kglite.CypherExecutionError, match="text_emb"):
        graph.cypher("MATCH ()-[r:CLAIMS]->() RETURN vector_score(r,'text_emb',[1.0,0.0]) AS score")
    with pytest.raises(kglite.CypherExecutionError, match="No relationship embedding store"):
        _query(graph, exact=True, top_k=1)


def test_failing_statement_restores_a_dropped_store() -> None:
    graph = _indexed_graph(3)
    with pytest.raises(kglite.CypherExecutionError, match="division by zero"):
        graph.cypher(
            "CALL db.relationship_embeddings.drop({type:'CLAIMS', text_column:'text'}) YIELD dropped "
            "WITH dropped MATCH (hub:Hub) SET hub.bad = 1/0 RETURN dropped"
        )

    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD count,dimension,metric RETURN count,dimension,metric"
    ).to_list() == [{"count": 3, "dimension": 2, "metric": "cosine"}]
    assert _query(graph, exact=True, top_k=1)[0]["score"] == pytest.approx(1.0)


def test_refresh_index_reports_the_pending_delta_and_clears_it() -> None:
    """``refreshed: 0`` is the only value the lifecycle test ever saw.

    With ``auto_refresh_limit: 0`` a vector write leaves the index stale rather
    than folding the change in, so the refresh has real work and must report
    the count it absorbed.
    """
    graph = _indexed_graph(5)
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text', "
        "auto_refresh_limit:0}) YIELD indexed RETURN indexed"
    )
    graph.cypher(
        "MATCH ()-[r:CLAIMS {rank:0}]->() "
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
        "entries:[{relationship:r, vector:[0.0,1.0]}]}) YIELD stored RETURN stored"
    )
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD index_state,delta RETURN index_state,delta"
    ).to_list() == [{"index_state": "stale", "delta": 1}]

    assert graph.cypher(
        "CALL db.relationship_embeddings.refresh_index({type:'CLAIMS', text_column:'text'}) YIELD refreshed RETURN "
        "refreshed"
    ).to_list() == [{"refreshed": 1}]
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD index_state,delta RETURN index_state,delta"
    ).to_list() == [{"index_state": "online", "delta": 0}]
    assert _query(graph, top_k=1)[0]["relationship"]["properties"]["rank"] == 1


def test_drop_index_is_false_when_the_store_carries_no_index() -> None:
    graph = _indexed_graph(2)
    assert graph.cypher(
        "CALL db.relationship_embeddings.drop_index({type:'CLAIMS', text_column:'text'}) YIELD dropped RETURN dropped"
    ).to_list() == [{"dropped": False}]
    assert graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD index_state,count RETURN index_state,count"
    ).to_list() == [{"index_state": "none", "count": 2}]


def _three_vector_graph() -> KnowledgeGraph:
    """Unit x, unit y, and a length-2 vector at 53° — so cosine, dot product and
    euclidean rank the same corpus three different ways."""
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (h:Hub {id: 0}), (h)-[:CLAIMS {rank: 1, text: 't'}]->(:Doc {id: 1}), "
        "(h)-[:CLAIMS {rank: 2, text: 't'}]->(:Doc {id: 2}), (h)-[:CLAIMS {rank: 3, text: 't'}]->(:Doc {id: 3})"
    )
    for rank, vector in [(1, [1.0, 0.0]), (2, [0.0, 1.0]), (3, [1.2, 1.6])]:
        graph.cypher(
            "MATCH ()-[r:CLAIMS {rank: $rank}]->() "
            "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
            "entries:[{relationship:r, vector:$vector}]}) YIELD stored RETURN stored",
            params={"rank": rank, "vector": vector},
        )
    return graph


@pytest.mark.parametrize(
    ("metric", "expected"),
    [
        # query [1, 0]: cos = x / |v|, dot = x, euclidean = -|v - q|
        ("cosine", {1: 1.0, 2: 0.0, 3: 0.6}),
        ("dot_product", {1: 1.0, 2: 0.0, 3: 1.2}),
        ("euclidean", {1: 0.0, 2: -math.sqrt(2.0), 3: -math.sqrt(0.04 + 2.56)}),
    ],
)
def test_relationship_scores_are_the_hand_computed_metric_values(metric: str, expected: dict) -> None:
    """Absolute goldens on a 3-vector corpus, through all three scoring routes.

    The existing tests assert ordering and self-similarity; a metric that
    silently resolved to another one (cosine for dot on unit vectors, say)
    kept every one of them green. The length-2 vector separates the metrics,
    and the three routes must agree with the hand computation, not merely with
    each other.
    """
    graph = _three_vector_graph()
    scalar = graph.cypher(
        "MATCH ()-[r:CLAIMS]->() RETURN r.rank AS rank, "
        "vector_score(r,'text_emb',[1.0,0.0],$metric) AS vector_score, "
        "text_score(r,'text',[1.0,0.0],$metric) AS text_score ORDER BY rank",
        params={"metric": metric},
    ).to_list()
    assert {row["rank"]: row["vector_score"] for row in scalar} == pytest.approx(expected, abs=1e-6)
    assert {row["rank"]: row["text_score"] for row in scalar} == pytest.approx(expected, abs=1e-6)

    procedure = graph.cypher(
        "CALL db.relationship_embeddings.query({type:'CLAIMS', text_column:'text', vector:[1.0,0.0], "
        "top_k:3, exact:true, metric:$metric}) YIELD relationship, score "
        "RETURN relationship, score",
        params={"metric": metric},
    ).to_list()
    assert {row["relationship"]["properties"]["rank"]: row["score"] for row in procedure} == pytest.approx(
        expected, abs=1e-6
    )
    assert [row["score"] for row in procedure] == sorted((row["score"] for row in procedure), reverse=True)
    assert graph.cypher(
        "MATCH ()-[r:CLAIMS]->() RETURN r.rank AS rank, embedding_norm(r,'text_emb') AS norm ORDER BY rank"
    ).to_list() == [
        {"rank": 1, "norm": pytest.approx(1.0)},
        {"rank": 2, "norm": pytest.approx(1.0)},
        {"rank": 3, "norm": pytest.approx(2.0)},
    ]


@pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])
def test_kgl_round_trip_keeps_relationship_vectors_in_every_storage_mode(mode: str, tmp_path: Path) -> None:
    """Saving and reloading a relationship store must not depend on the backend
    it was built on: the mapped and disk substrates serialise edge properties
    through different paths, and only the in-memory round trip was covered."""
    graph = _indexed_graph(6, _graph_in_mode(mode, tmp_path))
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )
    before_state = graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD entity,count,dimension,metric RETURN entity,count,dimension,metric"
    ).to_list()
    before_rows = [
        (row["relationship"]["properties"]["rank"], row["score"]) for row in _query(graph, exact=True, top_k=6)
    ]
    assert before_state == [{"entity": "relationship", "count": 6, "dimension": 2, "metric": "cosine"}]

    checkpoint = tmp_path / f"{mode}.kgl"
    graph.save(str(checkpoint))
    reopened = kglite.load(str(checkpoint))

    assert (
        reopened.cypher(
            "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
            "YIELD entity,count,dimension,metric RETURN entity,count,dimension,metric"
        ).to_list()
        == before_state
    )
    after_rows = [
        (row["relationship"]["properties"]["rank"], row["score"]) for row in _query(reopened, exact=True, top_k=6)
    ]
    assert [rank for rank, _ in after_rows] == [rank for rank, _ in before_rows]
    assert [score for _, score in after_rows] == pytest.approx([score for _, score in before_rows])


def _index_status(graph: KnowledgeGraph) -> list[dict]:
    return graph.cypher(
        "CALL db.relationship_embeddings.list({type:'CLAIMS', text_column:'text'}) "
        "YIELD index_state,delta RETURN index_state,delta"
    ).to_list()


def _kgl_metadata(blob: bytes) -> dict:
    size = int.from_bytes(blob[9:13], "little")
    return json.loads(blob[13 : 13 + size])


@pytest.mark.parametrize("storage", ["memory", "mapped"])
@pytest.mark.parametrize("source", ["memory", "mapped"])
def test_kgl_round_trip_keeps_the_relationship_hnsw_index_online(source: str, storage: str, tmp_path: Path) -> None:
    """The index is a derived cache, but rebuilding it on every open is a cost
    the node index never charged: a `.kgl` carries it, so a reload answers its
    first query through HNSW with the store reporting `online`. A disk graph
    never writes a `.kgl` (see the disk-generation test below)."""
    graph = _indexed_graph(6, _graph_in_mode(source, tmp_path))
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )
    exact = [row["relationship"]["properties"]["rank"] for row in _query(graph, exact=True, top_k=6)]

    checkpoint = tmp_path / f"{source}-{storage}.kgl"
    graph.save(str(checkpoint))
    reopened = kglite.load(str(checkpoint), storage=storage)

    assert _index_status(reopened) == [{"index_state": "online", "delta": 0}]
    first = _query(reopened, top_k=6)
    assert [row["search_method"] for row in first] == ["hnsw"] * 6
    assert [row["relationship"]["properties"]["rank"] for row in first][0] == exact[0]
    assert sorted(row["relationship"]["properties"]["rank"] for row in first) == sorted(exact)


@pytest.mark.parametrize("storage", ["memory", "mapped"])
def test_kgl_round_trip_keeps_a_stale_relationship_index_delta(storage: str, tmp_path: Path) -> None:
    """A store saved with an outstanding delta must reload still owing it — both
    an in-place replacement (dirty slot) and an appended vector (past the
    watermark). Restoring the index as covering everything would serve the
    replaced vector's old neighbourhood and never see the new one."""
    graph = _indexed_graph(5)
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text', "
        "auto_refresh_limit:0}) YIELD indexed RETURN indexed"
    )
    graph.cypher(
        "MATCH ()-[r:CLAIMS {rank:0}]->() "
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
        "entries:[{relationship:r, vector:[0.0,1.0]}]}) YIELD stored RETURN stored"
    )
    graph.cypher("MATCH (hub:Hub {id: 0}) CREATE (hub)-[:CLAIMS {rank: 99}]->(:Doc {id: 99})")
    graph.cypher(
        "MATCH ()-[r:CLAIMS {rank:99}]->() "
        "CALL db.relationship_embeddings.set({type:'CLAIMS', text_column:'text', "
        "entries:[{relationship:r, vector:[1.0,0.0]}]}) YIELD stored RETURN stored"
    )
    assert _index_status(graph) == [{"index_state": "stale", "delta": 2}]

    checkpoint = tmp_path / "stale.kgl"
    graph.save(str(checkpoint))
    reopened = kglite.load(str(checkpoint), storage=storage)

    assert _index_status(reopened) == [{"index_state": "stale", "delta": 2}]
    assert reopened.cypher(
        "CALL db.relationship_embeddings.refresh_index({type:'CLAIMS', text_column:'text'}) YIELD refreshed RETURN "
        "refreshed"
    ).to_list() == [{"refreshed": 2}]
    assert _index_status(reopened) == [{"index_state": "online", "delta": 0}]
    top = _query(reopened, top_k=1)[0]
    assert top["search_method"] == "hnsw"
    assert top["relationship"]["properties"]["rank"] == 99


def test_disk_generation_reopen_does_not_persist_the_relationship_index(tmp_path: Path) -> None:
    """Disk generations persist neither the node nor the relationship HNSW
    index: only `.kgl` carries it. `save()` on a disk graph writes a generation
    directory even at a `.kgl`-suffixed path (and `to_bytes()` refuses), so a
    disk-built graph always reopens with no index, keeping its vectors and
    falling back to the exact scan."""
    directory = tmp_path / "disk.kgl"
    graph = _indexed_graph(6, _graph_in_mode("disk", tmp_path))
    graph.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )
    graph.save(str(directory))
    del graph
    assert directory.is_dir()

    reopened = kglite.load(str(directory))
    # With no index every stored vector is outstanding, so `delta` is the store size.
    assert _index_status(reopened) == [{"index_state": "none", "delta": 6}]
    assert _query(reopened, top_k=1)[0]["search_method"] == "exact"


def test_kgl_without_a_relationship_index_carries_no_edge_vector_index_section() -> None:
    """The section is written only when a relationship index exists, so node-only
    files — and files whose relationship stores are unindexed — keep the bytes
    they wrote before the section existed (the golden digest in
    `test_phase4_parity.py` pins one such file)."""
    node_only = KnowledgeGraph()
    node_only.cypher("CREATE (:Doc {id: 1, title: 'a'})")
    unindexed = _indexed_graph(3)
    indexed = _indexed_graph(3)
    indexed.cypher(
        "CALL db.relationship_embeddings.build_index({type:'CLAIMS', text_column:'text'}) YIELD indexed RETURN indexed"
    )

    for graph in (node_only, unindexed):
        assert "edge_vector_index_compressed_size" not in _kgl_metadata(graph.to_bytes())
        assert "edge_vector_index" not in _kgl_metadata(graph.to_bytes()).get("section_digests", {})
    metadata = _kgl_metadata(indexed.to_bytes())
    assert metadata["edge_vector_index_compressed_size"] > 0
    assert "edge_vector_index" in metadata["section_digests"]
