"""Network-free claim/evidence GraphRAG with relationship embeddings."""

from __future__ import annotations

import argparse
import math
from pathlib import Path

import kglite


class TinyEmbedder:
    """Deterministic example model; production applications register their model."""

    dimension = 4
    model_id = "example/tiny-v1"

    def load(self) -> None:
        pass

    def unload(self) -> None:
        pass

    def embed(self, texts: list[str]) -> list[list[float]]:
        vectors: list[list[float]] = []
        for text in texts:
            buckets = [0.0] * self.dimension
            for position, byte in enumerate(text.casefold().encode()):
                buckets[position % self.dimension] += float(byte)
            norm = math.sqrt(sum(value * value for value in buckets)) or 1.0
            vectors.append([value / norm for value in buckets])
        return vectors


def exact_filtered(graph: kglite.KnowledgeGraph, question: str) -> list[dict]:
    return graph.cypher(
        """
        MATCH (claim:Claim)-[r:ASSERTS]->(evidence:Evidence)
        WHERE claim.collection = $collection AND r.confidence >= $minimum
        RETURN claim.id AS claim_id, evidence.id AS evidence_id,
               text_score(r, 'description', $question) AS score
        ORDER BY score DESC, claim_id, evidence_id
        """,
        params={"collection": "climate", "minimum": 0.8, "question": question},
    ).to_list()


def run(output: Path) -> None:
    graph = kglite.KnowledgeGraph()
    graph.set_embedder(TinyEmbedder())
    graph.cypher(
        """
        CREATE (c1:Claim {id:'c1', collection:'climate'}),
               (c2:Claim {id:'c2', collection:'other'}),
               (e1:Evidence {id:'e1'}), (e2:Evidence {id:'e2'}),
               (c1)-[:ASSERTS {description:'controlled heating increased evaporation',confidence:0.94}]->(e1),
               (c1)-[:ASSERTS {description:'warm windy days showed faster evaporation',confidence:0.81}]->(e2),
               (c2)-[:ASSERTS {description:'unrelated observation',confidence:0.88}]->(e2)
        """
    )
    report = graph.cypher(
        """
        MATCH (:Claim)-[r:ASSERTS]->(:Evidence)
        WITH collect(r) AS relationships
        CALL db.edge_embeddings.embed({type:'ASSERTS',text_property:'description',
          relationships:relationships,mode:'all'})
        YIELD embedded, skipped, dimension, model
        RETURN embedded, skipped, dimension, model
        """
    ).to_list()[0]
    assert report == {"embedded": 3, "skipped": 0, "dimension": 4, "model": TinyEmbedder.model_id}

    initial = exact_filtered(graph, "evidence that heat increases evaporation")
    assert len(initial) == 2
    assert {row["claim_id"] for row in initial} == {"c1"}
    assert all(row["score"] is not None for row in initial)

    refreshed = graph.cypher(
        """
        MATCH (:Claim {id:'c1'})-[r:ASSERTS]->(:Evidence {id:'e2'})
        SET r.description = 'warm and windy conditions increased evaporation rate'
        WITH collect(r) AS relationships
        CALL db.edge_embeddings.embed({type:'ASSERTS',text_property:'description',
          relationships:relationships,mode:'changed'})
        YIELD embedded, skipped RETURN embedded, skipped
        """
    ).to_list()[0]
    assert refreshed == {"embedded": 1, "skipped": 0}

    provenance = graph.cypher(
        "CALL db.edge_embeddings.list({type:'ASSERTS',text_property:'description'}) "
        "YIELD count,dimension,model RETURN count,dimension,model"
    ).to_list()
    assert provenance == [{"count": 3, "dimension": 4, "model": TinyEmbedder.model_id}]

    graph.cypher(
        "CALL db.edge_embeddings.build_index({type:'ASSERTS',text_property:'description'}) YIELD indexed RETURN indexed"
    )
    approximate = graph.cypher(
        "CALL db.edge_embeddings.query({type:'ASSERTS',text_property:'description',"
        "vector:$vector,top_k:2}) YIELD relationship,score,search_method "
        "RETURN relationship,score,search_method",
        params={"vector": TinyEmbedder().embed(["evidence that heat increases evaporation"])[0]},
    ).to_list()
    assert len(approximate) == 2
    assert all(row["relationship"]["type"] == "ASSERTS" for row in approximate)
    assert all(row["search_method"] == "hnsw" for row in approximate)

    graph.save(str(output))
    reopened = kglite.load(str(output))
    reopened.set_embedder(TinyEmbedder())
    rebuilt = reopened.cypher(
        "CALL db.edge_embeddings.build_index({type:'ASSERTS',text_property:'description'}) YIELD indexed RETURN indexed"
    ).to_list()
    assert rebuilt == [{"indexed": 3}]
    assert exact_filtered(reopened, "evidence that heat increases evaporation")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    run(parser.parse_args().output)
