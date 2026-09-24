"""Load knwler document-graph JSON into kglite and rank its relations.

knwler (github.com/Orbifold/knwler, MIT) extracts one JSON document per source
text: ``chunks`` with their text, and a ``graph`` of ``entities`` (name, type,
description, chunk_ids) and ``relations`` (source, target, type, description,
strength, chunk_ids). The two documents below reproduce that shape with
contents written for this example; no knwler file is copied.

The adapter mirrors knwler's own ``create_network``: an entity's id is
``name::type``, so the same name under two types stays two entities. Relations
become one relationship type per knwler relation type, documents link to their
chunks through ``CONTAINS``, and an endpoint that names no entity is an error,
not a silently created stub. The payoff is one ranked query across every
relation type (``db.relationship_embeddings.query`` with no ``type``).

Network-free: the embedder is a keyword stand-in. Run it with::

    python examples/knwler_import.py
"""

from __future__ import annotations

from collections import defaultdict

import kglite

DOCS = [
    {
        "id": "doc1",
        "title": "On the origin of species",
        "url": "https://example.org/origin",
        "language": "en",
        "chunks": [
            {"id": "doc1-chunk1", "document_id": "doc1", "text": "Charles Darwin was a naturalist."},
            {"id": "doc1-chunk2", "document_id": "doc1", "text": "Darwin proposed evolution by natural selection."},
        ],
        "graph": {
            "entities": [
                {"name": "Charles Darwin", "type": "Person", "description": "naturalist", "chunk_ids": ["doc1-chunk1"]},
                {
                    "name": "natural selection",
                    "type": "Concept",
                    "description": "mechanism of evolution",
                    "chunk_ids": ["doc1-chunk2"],
                },
            ],
            "relations": [
                {
                    "source": "Charles Darwin",
                    "target": "natural selection",
                    "type": "proposed",
                    "description": "Darwin proposed evolution by natural selection",
                    "strength": 0.9,
                    "chunk_ids": ["doc1-chunk2"],
                }
            ],
        },
    },
    {
        "id": "doc2",
        "title": "Radioactivity",
        "url": "https://example.org/radium",
        "language": "en",
        "chunks": [
            {"id": "doc2-chunk1", "document_id": "doc2", "text": "Marie Curie discovered radium."},
            {"id": "doc2-chunk2", "document_id": "doc2", "text": "Curie read Darwin's work on evolution."},
        ],
        "graph": {
            "entities": [
                {"name": "Marie Curie", "type": "Person", "description": "physicist", "chunk_ids": ["doc2-chunk1"]},
                {
                    "name": "radium",
                    "type": "Element",
                    "description": "radioactive element",
                    "chunk_ids": ["doc2-chunk1"],
                },
                {"name": "Charles Darwin", "type": "Person", "description": "naturalist", "chunk_ids": ["doc2-chunk2"]},
            ],
            "relations": [
                {
                    "source": "Marie Curie",
                    "target": "radium",
                    "type": "discovered",
                    "description": "Curie discovered the radioactive element radium",
                    "strength": 0.95,
                    "chunk_ids": ["doc2-chunk1"],
                },
                {
                    "source": "Marie Curie",
                    "target": "Charles Darwin",
                    "type": "read",
                    "description": "Curie read Darwin on evolution",
                    "strength": 0.3,
                    "chunk_ids": ["doc2-chunk2"],
                },
            ],
        },
    },
]


def knwler_to_spec(docs: list[dict]) -> dict:
    """A ``from_records`` spec for knwler documents."""
    entities: dict[str, dict] = {}
    documents: list[dict] = []
    chunks: list[dict] = []
    relations: dict[str, list[dict]] = defaultdict(list)
    for doc in docs:
        documents.append({"id": doc["id"], "title": doc.get("title"), "url": doc.get("url")})
        for chunk in doc.get("chunks", []):
            chunks.append({"id": chunk["id"], "document": chunk.get("document_id", doc["id"]), "text": chunk["text"]})
        type_of = {entity["name"]: entity["type"] for entity in doc["graph"]["entities"]}
        for entity in doc["graph"]["entities"]:
            key = f"{entity['name']}::{entity['type']}"
            merged = entities.setdefault(
                key,
                {
                    "id": key,
                    "name": entity["name"],
                    "type": entity["type"],
                    "description": entity.get("description", ""),
                },
            )
            merged.setdefault("chunk_ids", [])
            merged["chunk_ids"] += [c for c in entity.get("chunk_ids", []) if c not in merged["chunk_ids"]]
        for relation in doc["graph"]["relations"]:
            source_type = relation.get("source_type") or type_of.get(relation["source"], "")
            target_type = relation.get("target_type") or type_of.get(relation["target"], "")
            relations[relation["type"]].append(
                {
                    "src": f"{relation['source']}::{source_type}",
                    "tgt": f"{relation['target']}::{target_type}",
                    "description": relation.get("description", ""),
                    "strength": float(relation.get("strength", 0.5)),
                    "chunk_ids": list(relation.get("chunk_ids", [])),
                }
            )
    connections = [
        {
            "type": rel_type,
            "source_type": "Entity",
            "source_id_field": "src",
            "target_type": "Entity",
            "target_id_field": "tgt",
            "records": records,
        }
        for rel_type, records in sorted(relations.items())
    ]
    connections.append(
        {
            "type": "CONTAINS",
            "source_type": "Document",
            "source_id_field": "document",
            "target_type": "Chunk",
            "target_id_field": "id",
            "records": [{"document": chunk["document"], "id": chunk["id"]} for chunk in chunks],
        }
    )
    return {
        "nodes": [
            {"type": "Document", "id_field": "id", "title_field": "title", "records": documents},
            {"type": "Chunk", "id_field": "id", "records": chunks},
            {"type": "Entity", "id_field": "id", "title_field": "name", "records": list(entities.values())},
        ],
        "connections": connections,
    }


class KeywordEmbedder:
    """Counts a few keywords — a stand-in for a sentence-embedding model."""

    dimension = 5
    model_id = "example/keywords"
    WORDS = ("evolution", "darwin", "radium", "curie", "selection")

    def load(self) -> None:
        pass

    def unload(self) -> None:
        pass

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[float(word in text.lower()) + 0.01 for word in self.WORDS] for text in texts]


def run(question: str = "evolution by natural selection") -> dict:
    """Load the documents, embed every relation type, rank across them."""
    graph = kglite.from_records(knwler_to_spec(DOCS), on_missing_endpoint="error")
    graph.set_embedder(KeywordEmbedder())
    relation_types = sorted({relation["type"] for doc in DOCS for relation in doc["graph"]["relations"]})
    for rel_type in relation_types:
        graph.cypher(
            f"MATCH ()-[r:{rel_type}]->() WITH collect(r) AS rs "
            f"CALL db.relationship_embeddings.embed({{type: '{rel_type}', text_property: 'description', relationships: "
            f"rs}}) "
            "YIELD embedded RETURN embedded"
        )
    counts = {
        row["label"]: row["n"]
        for row in graph.cypher("MATCH (n) RETURN labels(n)[0] AS label, count(*) AS n").to_list()
    }
    ranked = graph.cypher(
        "CALL db.relationship_embeddings.query({text_property: 'description', text: $question, top_k: 3}) "
        "YIELD relationship, score, type "
        "RETURN startNode(relationship).name AS source, type, endNode(relationship).name AS target, score",
        params={"question": question},
    ).to_list()
    return {"graph": graph, "counts": counts, "relation_types": relation_types, "ranked": ranked}


def main() -> None:
    result = run()
    print("nodes:", result["counts"])
    print("relation types:", result["relation_types"])
    for row in result["ranked"]:
        print(f"{row['score']:.3f}  {row['source']} -[{row['type']}]-> {row['target']}")


if __name__ == "__main__":
    main()
