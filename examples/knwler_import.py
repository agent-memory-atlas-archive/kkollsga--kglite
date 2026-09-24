"""Load knwler document-graph JSON into kglite and rank its relations.

knwler (github.com/Orbifold/knwler, MIT) writes two JSON shapes, and the
adapter takes both:

* **per-document** — one JSON per source text: ``chunks`` with their text, and
  a ``graph`` of ``entities`` (name, type, description, chunk_ids) and
  ``relations`` (source, source_type, target, target_type, type, description,
  strength, chunk_ids). Pass one such dict, or a list of them.
* **consolidated** — the merged ``consolidated_graph.json``:
  ``{id, documents, schema, graph: {entities, relations, clusters}, chunks}``,
  where each chunk names its document and ``clusters`` groups entities as
  ``"name::type"`` members.

The documents below reproduce the per-document shape with contents written for
this example; no knwler file is copied. :func:`consolidate` builds the
consolidated shape from them the way knwler's consolidation merges entities.

The adapter mirrors knwler's own ``create_network``: an ``Entity`` node's id is
``name::type``, so the same name under two types stays two nodes. Relations
become one relationship type per knwler relation type (endpoint types come
from the relation's own ``source_type`` / ``target_type``, else from the
document's entities), documents link to their chunks through ``CONTAINS``,
chunks to the ``Entity`` nodes they mention through ``HAS_ENTITY``, and those
nodes to their clusters through ``BELONGS_TO``. An endpoint that names no
``Entity`` node is an error, not a silently created stub. The payoff is one ranked query across every
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


def _is_consolidated(data: dict | list[dict]) -> bool:
    return isinstance(data, dict) and "documents" in data


def _documents(data: dict | list[dict]) -> list[dict]:
    """Per-document input as a list: one dict, or a list of them."""
    return [data] if isinstance(data, dict) else list(data)


def _entity_key(name: str, entity_type: str) -> str:
    return f"{name}::{entity_type}"


def _parts(data: dict | list[dict]) -> tuple[list[dict], list[dict], list[dict], list[dict], list[dict]]:
    """``(documents, chunks, entities, relations, clusters)`` with every
    relation's endpoint types filled in."""
    if _is_consolidated(data):
        graph = data["graph"]
        type_of: dict[str, set[str]] = defaultdict(set)
        for entity in graph["entities"]:
            type_of[entity["name"]].add(entity["type"])
        relations = [_typed(relation, type_of) for relation in graph["relations"]]
        return data["documents"], data.get("chunks", []), graph["entities"], relations, graph.get("clusters", [])
    documents, chunks, entities, relations = [], [], [], []
    for doc in _documents(data):
        documents.append({"id": doc["id"], "title": doc.get("title"), "url": doc.get("url")})
        chunks += [{**chunk, "document": _chunk_document(chunk, doc["id"])} for chunk in doc.get("chunks", [])]
        type_of = defaultdict(set)
        for entity in doc["graph"]["entities"]:
            type_of[entity["name"]].add(entity["type"])
        entities += doc["graph"]["entities"]
        relations += [_typed(relation, type_of) for relation in doc["graph"]["relations"]]
    return documents, chunks, entities, relations, []


def _chunk_document(chunk: dict, default: str | None = None) -> str | None:
    return chunk.get("document") or chunk.get("document_id") or default


def _typed(relation: dict, type_of: dict[str, set[str]]) -> dict:
    """The relation with ``source_type`` / ``target_type`` set: its own, else
    the one type its endpoint name has (an ambiguous name stays unresolved and
    fails as a missing endpoint)."""
    typed = dict(relation)
    for side in ("source", "target"):
        if not typed.get(f"{side}_type"):
            candidates = type_of.get(relation[side], set())
            typed[f"{side}_type"] = next(iter(candidates)) if len(candidates) == 1 else ""
    return typed


def knwler_to_spec(data: dict | list[dict]) -> dict:
    """A ``from_records`` spec for knwler output: a consolidated export, one
    per-document JSON, or a list of per-document JSONs."""
    documents, chunks, raw_entities, raw_relations, clusters = _parts(data)
    entities: dict[str, dict] = {}
    for entity in raw_entities:
        key = _entity_key(entity["name"], entity["type"])
        merged = entities.setdefault(
            key,
            {"id": key, "name": entity["name"], "type": entity["type"], "description": entity.get("description", "")},
        )
        merged.setdefault("chunk_ids", [])
        merged["chunk_ids"] += [c for c in entity.get("chunk_ids", []) if c not in merged["chunk_ids"]]
    relations: dict[str, list[dict]] = defaultdict(list)
    for relation in raw_relations:
        relations[relation["type"]].append(
            {
                "src": _entity_key(relation["source"], relation["source_type"]),
                "tgt": _entity_key(relation["target"], relation["target_type"]),
                "description": relation.get("description", ""),
                "strength": float(relation.get("strength", 0.5)),
                "chunk_ids": list(relation.get("chunk_ids", [])),
            }
        )
    chunk_rows = [{"id": chunk["id"], "document": _chunk_document(chunk), "text": chunk["text"]} for chunk in chunks]
    chunk_ids = {chunk["id"] for chunk in chunk_rows}
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
            "records": [{"document": chunk["document"], "id": chunk["id"]} for chunk in chunk_rows],
        }
    )
    connections.append(
        {
            "type": "HAS_ENTITY",
            "source_type": "Chunk",
            "source_id_field": "chunk",
            "target_type": "Entity",
            "target_id_field": "entity",
            "records": [
                {"chunk": chunk_id, "entity": entity["id"]}
                for entity in entities.values()
                for chunk_id in entity["chunk_ids"]
                if chunk_id in chunk_ids
            ],
        }
    )
    nodes = [
        {"type": "Document", "id_field": "id", "title_field": "title", "records": documents},
        {"type": "Chunk", "id_field": "id", "records": chunk_rows},
        {"type": "Entity", "id_field": "id", "title_field": "name", "records": list(entities.values())},
    ]
    if clusters:
        nodes.append(
            {
                "type": "Cluster",
                "id_field": "id",
                "records": [
                    {"id": f"cluster-{c['id']}", "topics": c.get("topics", []), "description": c.get("description", "")}
                    for c in clusters
                ],
            }
        )
        connections.append(
            {
                "type": "BELONGS_TO",
                "source_type": "Entity",
                "source_id_field": "entity",
                "target_type": "Cluster",
                "target_id_field": "cluster",
                "records": [
                    {"entity": member, "cluster": f"cluster-{c['id']}"}
                    for c in clusters
                    for member in c.get("members", [])
                ],
            }
        )
    return {"nodes": nodes, "connections": connections}


def consolidate(docs: list[dict]) -> dict:
    """knwler's consolidated shape built from per-document dicts: entities merged
    by (name, type), relations by (source, source_type, target, target_type,
    type), chunks carrying their document, one cluster per entity type."""
    entities: dict[str, dict] = {}
    relations: dict[tuple, dict] = {}
    for doc in docs:
        type_of: dict[str, set[str]] = defaultdict(set)
        for entity in doc["graph"]["entities"]:
            type_of[entity["name"]].add(entity["type"])
            merged = entities.setdefault(
                _entity_key(entity["name"], entity["type"]),
                {**entity, "chunk_ids": []},
            )
            merged["chunk_ids"] += [c for c in entity.get("chunk_ids", []) if c not in merged["chunk_ids"]]
        for relation in doc["graph"]["relations"]:
            typed = _typed(relation, type_of)
            key = (typed["source"], typed["source_type"], typed["target"], typed["target_type"], typed["type"])
            relations.setdefault(key, typed)
    clusters: dict[str, list[str]] = defaultdict(list)
    for key, entity in entities.items():
        clusters[entity["type"]].append(key)
    return {
        "id": "consolidated",
        "documents": [{"id": d["id"], "title": d.get("title"), "url": d.get("url")} for d in docs],
        "schema": {},
        "graph": {
            "entities": list(entities.values()),
            "relations": list(relations.values()),
            "clusters": [
                {"id": index, "topics": [entity_type], "description": f"{entity_type} entities", "members": members}
                for index, (entity_type, members) in enumerate(sorted(clusters.items()))
            ],
        },
        "chunks": [
            {**chunk, "document": _chunk_document(chunk, d["id"])} for d in docs for chunk in d.get("chunks", [])
        ],
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


def run(question: str = "evolution by natural selection", data: dict | list[dict] = DOCS) -> dict:
    """Load knwler output (per-document or consolidated), embed every relation
    type in one call, rank across them."""
    graph = kglite.from_records(knwler_to_spec(data), on_missing_endpoint="error")
    graph.set_embedder(KeywordEmbedder())
    _, _, _, raw_relations, _ = _parts(data)
    relation_types = sorted({relation["type"] for relation in raw_relations})
    graph.cypher(
        "MATCH ()-[r]->() WHERE type(r) IN $types WITH collect(r) AS rs "
        "CALL db.relationship_embeddings.embed({types: $types, text_column: 'description', relationships: rs}) "
        "YIELD embedded RETURN embedded",
        params={"types": relation_types},
    )
    counts = {
        row["label"]: row["n"]
        for row in graph.cypher("MATCH (n) RETURN labels(n)[0] AS label, count(*) AS n").to_list()
    }
    ranked = graph.cypher(
        "CALL db.relationship_embeddings.query({text_column: 'description', text: $question, top_k: 3}) "
        "YIELD relationship, score, type "
        "RETURN startNode(relationship).name AS source, type, endNode(relationship).name AS target, score",
        params={"question": question},
    ).to_list()
    return {"graph": graph, "counts": counts, "relation_types": relation_types, "ranked": ranked}


def main() -> None:
    result = run()
    print("nodes:", result["counts"])
    print("from the consolidated shape:", run(data=consolidate(DOCS))["counts"])
    print("relation types:", result["relation_types"])
    for row in result["ranked"]:
        print(f"{row['score']:.3f}  {row['source']} -[{row['type']}]-> {row['target']}")


if __name__ == "__main__":
    main()
