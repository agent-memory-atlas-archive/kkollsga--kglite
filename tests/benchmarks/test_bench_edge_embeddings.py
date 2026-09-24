"""Relationship-embedding cells paired with their node twins.

Not a core cell: CI runs ``test_bench_core.py`` unmodified under the
published 0.13.2 wheel, where ``db.edge_embeddings.*`` does not exist, so a
relationship cell can never live there. Each cell here instead carries a
**self-contained ratio guard** against its node twin measured in the same
process, which needs no baseline row. Run explicitly, release build only::

    uv run --no-sync maturin develop --release
    pytest tests/benchmarks/test_bench_edge_embeddings.py -m benchmark -v -s

Three shapes, mirroring the node harness:

* **Exact scan scoring** — every relationship of a type scored against a raw
  query vector through ``vector_score`` (twin:
  ``test_bench_vector_score_scan_100k_384``). Deterministic, ``min``.
* **Store query, exact vs HNSW** — ``db.edge_embeddings.query`` on a
  10k x 128 store with a recall@10 oracle computed outside the timed region
  (twin: ``test_bench_hnsw_search`` / ``test_bench_exact_vector_search``).
  ``min``.
* **Cross-type query** — ``db.edge_embeddings.query({types: [...]})`` over
  three stores that split the 10k x 128 query corpus, against the
  single-store query on the same total (its twin is the single store, not a
  node path). ``min``.
* **Ingest through the embedder** — ``db.edge_embeddings.embed`` filling a
  fresh store from a deterministic model (twin: ``embed_texts``). Each round
  ingests into a fresh graph, a once-per-event cost, so the **mean** of
  first writes is the statistic (Performance protocol item 4a).

The ratio ceilings are the program's stop rule (plan D7): a relationship
path more than ``MAX_RATIO`` slower than its node twin is a finding, not a
tolerance to raise.
"""

from __future__ import annotations

import time

import numpy as np
import pandas as pd
import pytest

import kglite

SCAN_N = 100_000
SCAN_DIMENSION = 384
QUERY_N = 10_000
QUERY_DIMENSION = 128
INGEST_N = 20_000
INGEST_DIMENSION = 384
LATENT_DIMENSION = 16
TOP_K = 10
RECALL_FLOOR = 0.90
SET_BATCH = 5_000
SCAN_ROUNDS = 30
SEARCH_ROUNDS = 100
SEARCH_WARMUP_ROUNDS = 20
INGEST_ROUNDS = 3
#: Relationship path may cost at most this multiple of its node twin.
MAX_RATIO = 1.5


def _vectors(n: int, dimension: int, seed: int) -> np.ndarray:
    rng = np.random.default_rng(seed)
    latent = rng.standard_normal((n, LATENT_DIMENSION), dtype=np.float32)
    projection = rng.standard_normal((LATENT_DIMENSION, dimension), dtype=np.float32)
    return np.asarray(latent @ projection, dtype=np.float32)


def _min_seconds(fn, rounds: int, warmup: int = 3) -> float:
    for _ in range(warmup):
        fn()
    best = float("inf")
    for _ in range(rounds):
        started = time.perf_counter()
        fn()
        best = min(best, time.perf_counter() - started)
    return best


def _twin_graphs(n: int, dimension: int, seed: int) -> tuple[kglite.KnowledgeGraph, kglite.KnowledgeGraph, np.ndarray]:
    """A node store and a relationship store holding the same vectors.

    The relationship graph hangs one ``CLAIMS`` edge per document off a hub,
    and the vectors are installed through ``db.edge_embeddings.set`` in
    batches (a single 100k-entry parameter list is a ~1 GB transient).
    """
    vectors = _vectors(n, dimension, seed)
    frame = pd.DataFrame(
        {
            "id": np.arange(n, dtype=np.int64),
            "title": [f"d{i}" for i in range(n)],
            "summary": [f"text {i}" for i in range(n)],
        }
    )

    nodes = kglite.KnowledgeGraph()
    nodes.add_nodes(frame, "Doc", "id", "title")
    nodes.set_embeddings("Doc", "summary", dict(enumerate(vectors)), metric="cosine")

    edges = kglite.KnowledgeGraph()
    edges.add_nodes(pd.DataFrame({"id": [0], "title": ["hub"]}), "Hub", "id", "title")
    edges.add_nodes(frame, "Doc", "id", "title")
    edges.add_connections(
        pd.DataFrame(
            {"hub": np.zeros(n, dtype=np.int64), "doc": np.arange(n, dtype=np.int64), "summary": frame["summary"]}
        ),
        "CLAIMS",
        "Hub",
        "hub",
        "Doc",
        "doc",
    )
    for start in range(0, n, SET_BATCH):
        batch = [{"id": int(i), "vector": vectors[i].tolist()} for i in range(start, min(start + SET_BATCH, n))]
        stored = edges.cypher(
            "UNWIND $batch AS entry MATCH (:Hub)-[r:CLAIMS]->(:Doc {id: entry.id}) "
            "WITH collect({relationship: r, vector: entry.vector}) AS entries "
            "CALL db.edge_embeddings.set({type:'CLAIMS', text_property:'summary', entries: entries, metric:'cosine'}) "
            "YIELD stored RETURN stored",
            params={"batch": batch},
        ).to_list()
        # `stored` is the store's size after the write (the per-call count is
        # `changed`), so it climbs by one batch each round.
        assert stored == [{"stored": min(start + SET_BATCH, n)}]
    return nodes, edges, vectors


@pytest.fixture(scope="module")
def scan_twins():
    return _twin_graphs(SCAN_N, SCAN_DIMENSION, seed=20_260_924)


@pytest.fixture(scope="module")
def query_twins():
    nodes, edges, vectors = _twin_graphs(QUERY_N, QUERY_DIMENSION, seed=20_260_925)
    nodes.build_vector_index("Doc", "summary")
    assert edges.cypher(
        "CALL db.edge_embeddings.build_index({type:'CLAIMS', text_property:'summary'}) YIELD indexed RETURN indexed"
    ).to_list() == [{"indexed": QUERY_N}]
    return nodes, edges, vectors


def _edge_query(edges: kglite.KnowledgeGraph, query: list[float], *, exact: bool) -> list[int]:
    rows = edges.cypher(
        "CALL db.edge_embeddings.query({type:'CLAIMS', text_property:'summary', vector:$q, top_k:$k, exact:$exact}) "
        "YIELD relationship, search_method RETURN endNode(relationship).id AS end, search_method",
        params={"q": query, "k": TOP_K, "exact": exact},
    ).to_list()
    assert {row["search_method"] for row in rows} == {"exact" if exact else "hnsw"}
    return [int(row["end"]) for row in rows]


@pytest.mark.benchmark
def test_bench_edge_vector_score_scan_100k_384(benchmark, scan_twins):
    """Whole-type exact scan through ``vector_score`` against the node twin."""
    nodes, edges, vectors = scan_twins
    query = vectors[SCAN_N // 2].tolist()
    edge_scan = lambda: edges.cypher(  # noqa: E731
        "MATCH (:Hub)-[r:CLAIMS]->(d:Doc) RETURN d.id AS id, vector_score(r,'summary_emb',$q) AS s "
        "ORDER BY s DESC LIMIT 10",
        params={"q": query},
    ).to_list()
    node_scan = lambda: nodes.cypher(  # noqa: E731
        "MATCH (n:Doc) RETURN n.id AS id, vector_score(n,'summary_emb',$q) AS s ORDER BY s DESC LIMIT 10",
        params={"q": query},
    ).to_list()
    assert edge_scan()[0]["id"] == SCAN_N // 2, "the self-hit must rank first"
    store_route = lambda: edges.cypher(  # noqa: E731
        "CALL db.edge_embeddings.query({type:'CLAIMS', text_property:'summary', vector:$q, top_k:10, exact:true}) "
        "YIELD relationship, score RETURN endNode(relationship).id AS id, score",
        params={"q": query},
    ).to_list()
    assert store_route()[0]["id"] == SCAN_N // 2
    node_min = _min_seconds(node_scan, SCAN_ROUNDS)
    store_min = _min_seconds(store_route, SCAN_ROUNDS)
    rows = benchmark.pedantic(edge_scan, rounds=SCAN_ROUNDS, iterations=1, warmup_rounds=3)
    assert rows[0]["id"] == SCAN_N // 2
    edge_min = min(benchmark.stats.stats.data)
    benchmark.extra_info.update(
        {
            "node_twin_min_s": node_min,
            "edge_over_node": edge_min / node_min,
            "edge_store_route_min_s": store_min,
            "edge_store_route_over_node": store_min / node_min,
        }
    )
    assert edge_min <= MAX_RATIO * node_min, f"relationship scan {edge_min / node_min:.2f}x its node twin"


@pytest.mark.benchmark
@pytest.mark.parametrize("exact", [True, False], ids=["exact", "hnsw"])
def test_bench_edge_query_10k_128(benchmark, query_twins, exact):
    """``db.edge_embeddings.query`` against the node ``vector_search`` twin, with a recall oracle."""
    nodes, edges, vectors = query_twins
    query_ids = [QUERY_N // 4 + 37 * i for i in range(20)]
    if not exact:
        hits = 0
        for qid in query_ids:
            truth = set(_edge_query(edges, vectors[qid].tolist(), exact=True))
            hits += len(truth & set(_edge_query(edges, vectors[qid].tolist(), exact=False)))
        recall = hits / (len(query_ids) * TOP_K)
        assert recall >= RECALL_FLOOR, f"recall@{TOP_K} {recall:.3f}"
        benchmark.extra_info["recall_at_10"] = recall
    query = vectors[query_ids[10]].tolist()
    node_query = lambda: nodes.select("Doc").vector_search("summary", query, top_k=TOP_K, exact=exact)  # noqa: E731
    node_min = _min_seconds(node_query, SEARCH_ROUNDS, warmup=SEARCH_WARMUP_ROUNDS)
    rows = benchmark.pedantic(
        _edge_query,
        args=(edges, query),
        kwargs={"exact": exact},
        rounds=SEARCH_ROUNDS,
        iterations=1,
        warmup_rounds=SEARCH_WARMUP_ROUNDS,
    )
    assert len(rows) == TOP_K and rows[0] == query_ids[10]
    edge_min = min(benchmark.stats.stats.data)
    benchmark.extra_info.update({"node_twin_min_s": node_min, "edge_over_node": edge_min / node_min})
    assert edge_min <= MAX_RATIO * node_min, f"relationship query {edge_min / node_min:.2f}x its node twin"


CROSS_TYPES = ("CLAIMS_A", "CLAIMS_B", "CLAIMS_C")


@pytest.fixture(scope="module")
def cross_type_graph(query_twins):
    """The query corpus split across three relationship types by ``id % 3``,
    each an indexed store — the same total the single-store cell ranks."""
    _, _, vectors = query_twins
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"id": [0], "title": ["hub"]}), "Hub", "id", "title")
    ids = np.arange(QUERY_N, dtype=np.int64)
    graph.add_nodes(pd.DataFrame({"id": ids, "title": [f"d{i}" for i in ids]}), "Doc", "id", "title")
    for offset, rel_type in enumerate(CROSS_TYPES):
        members = ids[ids % 3 == offset]
        graph.add_connections(
            pd.DataFrame({"hub": np.zeros(len(members), dtype=np.int64), "doc": members}),
            rel_type,
            "Hub",
            "hub",
            "Doc",
            "doc",
        )
        for start in range(0, len(members), SET_BATCH):
            batch = [{"id": int(i), "vector": vectors[i].tolist()} for i in members[start : start + SET_BATCH]]
            graph.cypher(
                f"UNWIND $batch AS entry MATCH (:Hub)-[r:{rel_type}]->(:Doc {{id: entry.id}}) "
                "WITH collect({relationship: r, vector: entry.vector}) AS entries "
                f"CALL db.edge_embeddings.set({{type:'{rel_type}', text_property:'summary', entries: entries, "
                "metric:'cosine'}) YIELD stored RETURN stored",
                params={"batch": batch},
            )
        graph.cypher(
            f"CALL db.edge_embeddings.build_index({{type:'{rel_type}', text_property:'summary'}}) "
            "YIELD indexed RETURN indexed"
        )
    return graph


def _cross_type_query(graph: kglite.KnowledgeGraph, query: list[float], *, exact: bool) -> list[int]:
    rows = graph.cypher(
        "CALL db.edge_embeddings.query({types:$types, text_property:'summary', vector:$q, top_k:$k, exact:$exact}) "
        "YIELD relationship, search_method RETURN endNode(relationship).id AS end, search_method",
        params={"types": list(CROSS_TYPES), "q": query, "k": TOP_K, "exact": exact},
    ).to_list()
    assert {row["search_method"] for row in rows} == {"exact" if exact else "hnsw"}
    return [int(row["end"]) for row in rows]


@pytest.mark.benchmark
@pytest.mark.parametrize("exact", [True, False], ids=["exact", "hnsw"])
def test_bench_edge_cross_type_query_3x_10k_128(benchmark, query_twins, cross_type_graph, exact):
    """A three-store merged query against the single-store query on the same corpus."""
    _, edges, vectors = query_twins
    query = vectors[QUERY_N // 4 + 370].tolist()
    exact_truth = _cross_type_query(cross_type_graph, query, exact=True)
    assert exact_truth == _edge_query(edges, query, exact=True), "the merge must equal the single store"
    single_min = _min_seconds(
        lambda: _edge_query(edges, query, exact=exact), SEARCH_ROUNDS, warmup=SEARCH_WARMUP_ROUNDS
    )
    rows = benchmark.pedantic(
        _cross_type_query,
        args=(cross_type_graph, query),
        kwargs={"exact": exact},
        rounds=SEARCH_ROUNDS,
        iterations=1,
        warmup_rounds=SEARCH_WARMUP_ROUNDS,
    )
    assert rows[0] == QUERY_N // 4 + 370
    merged_min = min(benchmark.stats.stats.data)
    benchmark.extra_info.update({"single_store_min_s": single_min, "merged_over_single": merged_min / single_min})
    # Exact ranks the same 10k vectors either way, so the merge must be free.
    # An HNSW search costs its ef-bound walk, not its store size, so three
    # stores cost three searches (release: 1.72x); the bound is one search per
    # store — a merge that re-ranks every candidate, or an unindexed store
    # (search_method already pins hnsw per row), is what it catches.
    limit = MAX_RATIO if exact else float(len(CROSS_TYPES))
    assert merged_min <= limit * single_min, f"cross-type query {merged_min / single_min:.2f}x the single store"


class _MatrixEmbedder:
    """Deterministic model: the text's integer suffix indexes a precomputed matrix."""

    def __init__(self, matrix: np.ndarray) -> None:
        self.matrix = matrix
        self.dimension = int(matrix.shape[1])
        self.model_id = "bench/matrix"

    def load(self) -> None:
        pass

    def unload(self) -> None:
        pass

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [self.matrix[int(text.rsplit(" ", 1)[1])].tolist() for text in texts]


@pytest.mark.benchmark
def test_bench_edge_embed_ingest_20k_384(benchmark):
    """Fresh-store ingest through ``db.edge_embeddings.embed`` against ``embed_texts``."""
    vectors = _vectors(INGEST_N, INGEST_DIMENSION, seed=20_260_926)
    frame = pd.DataFrame(
        {
            "id": np.arange(INGEST_N, dtype=np.int64),
            "title": [f"d{i}" for i in range(INGEST_N)],
            "summary": [f"text {i}" for i in range(INGEST_N)],
        }
    )

    def fresh_edges() -> kglite.KnowledgeGraph:
        g = kglite.KnowledgeGraph()
        g.add_nodes(pd.DataFrame({"id": [0], "title": ["hub"]}), "Hub", "id", "title")
        g.add_nodes(frame, "Doc", "id", "title")
        g.add_connections(
            pd.DataFrame({"hub": np.zeros(INGEST_N, dtype=np.int64), "doc": frame["id"], "summary": frame["summary"]}),
            "CLAIMS",
            "Hub",
            "hub",
            "Doc",
            "doc",
        )
        g.set_embedder(_MatrixEmbedder(vectors))
        return g

    def fresh_nodes() -> kglite.KnowledgeGraph:
        g = kglite.KnowledgeGraph()
        g.add_nodes(frame, "Doc", "id", "title")
        g.set_embedder(_MatrixEmbedder(vectors))
        return g

    def edge_ingest(g: kglite.KnowledgeGraph):
        return g.cypher(
            "MATCH ()-[r:CLAIMS]->() WITH collect(r) AS rs "
            "CALL db.edge_embeddings.embed({type:'CLAIMS', text_property:'summary', "
            "relationships: rs, mode:'missing'}) "
            "YIELD embedded RETURN embedded"
        ).to_list()

    node_means = []
    for _ in range(INGEST_ROUNDS):
        g = fresh_nodes()
        started = time.perf_counter()
        g.embed_texts("Doc", "summary", show_progress=False)
        node_means.append(time.perf_counter() - started)
    node_mean = sum(node_means) / len(node_means)

    result = benchmark.pedantic(edge_ingest, setup=lambda: ((fresh_edges(),), {}), rounds=INGEST_ROUNDS, iterations=1)
    assert result == [{"embedded": INGEST_N}]
    edge_mean = sum(benchmark.stats.stats.data) / len(benchmark.stats.stats.data)
    benchmark.extra_info.update(
        {"node_twin_mean_s": node_mean, "edge_over_node": edge_mean / node_mean, "statistic": "mean-of-first-writes"}
    )
    assert edge_mean <= MAX_RATIO * node_mean, f"relationship ingest {edge_mean / node_mean:.2f}x its node twin"
