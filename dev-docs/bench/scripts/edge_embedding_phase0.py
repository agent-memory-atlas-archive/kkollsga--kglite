#!/usr/bin/env python3
"""Focused node-embedding control used by the edge-embedding program.

Run against an already installed release wheel. Output is one JSON document;
the execution record describes how reference and candidate environments are
provisioned.
"""

import json
import math
import statistics
import time

import kglite

NODES = 10_000
DIMENSION = 16
WARMUP = 20


def graph(with_vectors: bool) -> kglite.KnowledgeGraph:
    result = kglite.KnowledgeGraph()
    result.cypher(
        "UNWIND range(0, $last) AS i CREATE (:Doc {id:i, text:toString(i)})",
        params={"last": NODES - 1},
    )
    if with_vectors:
        result.set_embeddings(
            "Doc",
            "text",
            {i: [math.sin(i + j) * 0.5 for j in range(DIMENSION)] for i in range(NODES)},
        )
    return result


def samples(operation, repetitions: int) -> dict[str, float | int]:
    for _ in range(WARMUP):
        operation()
    elapsed = []
    for _ in range(repetitions):
        started = time.perf_counter_ns()
        operation()
        elapsed.append((time.perf_counter_ns() - started) / 1_000)
    return {
        "n": repetitions,
        "min_us": min(elapsed),
        "median_us": statistics.median(elapsed),
        "mean_us": statistics.mean(elapsed),
    }


def run() -> dict:
    embedded = graph(True)
    query = [0.25] * DIMENSION
    result = {
        "nodes": NODES,
        "dimension": DIMENSION,
        "warmup": WARMUP,
        "vector_search_exact": samples(
            lambda: embedded.select("Doc").vector_search("text", query, top_k=10, exact=True), 200
        ),
        "cypher_vector_score_exact": samples(
            lambda: embedded.cypher(
                "MATCH (n:Doc) RETURN n.id AS id, "
                "vector_score(n, 'text_emb', $q, 'cosine', {exact:true}) AS score "
                "ORDER BY score DESC LIMIT 10",
                params={"q": query},
            ).to_list(),
            200,
        ),
        "embedding_info": samples(lambda: embedded.embedding_info("Doc", "text"), 400),
        "node_delete": {},
    }
    for name, with_vectors in (("no_vectors", False), ("with_vectors", True)):
        mutation_graph = graph(with_vectors)
        next_id = 0

        def delete() -> None:
            nonlocal next_id
            mutation_graph.cypher("MATCH (n:Doc {id:$id}) DETACH DELETE n", params={"id": next_id})
            next_id += 1

        result["node_delete"][name] = samples(delete, 120)
    return result


if __name__ == "__main__":
    print(json.dumps(run(), sort_keys=True))
