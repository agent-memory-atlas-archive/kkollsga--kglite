"""The knwler import example is executable, network-free, and loads what it claims."""

from __future__ import annotations

import copy
import importlib.util
from pathlib import Path

import pytest

import kglite


def _load_example():
    source = Path(__file__).parents[1] / "examples" / "knwler_import.py"
    spec = importlib.util.spec_from_file_location("knwler_import", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_example_loads_the_documents_and_ranks_across_relation_types() -> None:
    result = _load_example().run()
    # Darwin appears in both documents under the same type: one entity.
    assert result["counts"] == {"Document": 2, "Chunk": 4, "Entity": 4}
    assert result["relation_types"] == ["discovered", "proposed", "read"]
    graph = result["graph"]
    assert graph.cypher(
        "MATCH (d:Document {id: 'doc2'})-[:CONTAINS]->(c:Chunk) RETURN c.id AS id ORDER BY id"
    ).to_list() == [{"id": "doc2-chunk1"}, {"id": "doc2-chunk2"}]
    assert graph.cypher(
        "MATCH (:Entity {name: 'Marie Curie'})-[r:read]->(t:Entity) RETURN t.id AS target, r.strength AS w"
    ).to_list() == [{"target": "Charles Darwin::Person", "w": 0.3}]
    ranked = result["ranked"]
    assert [(row["source"], row["type"], row["target"]) for row in ranked] == [
        ("Charles Darwin", "proposed", "natural selection"),
        ("Marie Curie", "read", "Charles Darwin"),
        ("Marie Curie", "discovered", "radium"),
    ]
    assert ranked[0]["score"] > ranked[1]["score"] > ranked[2]["score"]


def test_an_endpoint_naming_no_entity_is_an_error() -> None:
    module = _load_example()
    docs = copy.deepcopy(module.DOCS)
    docs[1]["graph"]["relations"][0]["target"] = "polonium"
    with pytest.raises(Exception, match="polonium"):
        kglite.from_records(module.knwler_to_spec(docs), on_missing_endpoint="error")


def test_the_example_runs_as_a_script(capsys: pytest.CaptureFixture[str]) -> None:
    _load_example().main()
    out = capsys.readouterr().out
    assert "Charles Darwin -[proposed]-> natural selection" in out


def test_the_consolidated_export_loads_every_document_chunk_and_cluster() -> None:
    """``consolidated_graph.json`` is a dict with ``documents`` / ``graph`` /
    ``chunks``. Passed to the per-document-only adapter it raised a
    ``TypeError``, and wrapped in a list it loaded one Document and hung every
    chunk on the consolidated id."""
    module = _load_example()
    consolidated = module.consolidate(module.DOCS)
    assert set(consolidated) == {"id", "documents", "schema", "graph", "chunks"}
    result = module.run(data=consolidated)
    assert result["counts"] == {"Document": 2, "Chunk": 4, "Entity": 4, "Cluster": 3}
    graph = result["graph"]
    assert graph.cypher(
        "MATCH (d:Document)-[:CONTAINS]->(c:Chunk) RETURN d.id AS doc, count(c) AS n ORDER BY doc"
    ).to_list() == [{"doc": "doc1", "n": 2}, {"doc": "doc2", "n": 2}]
    assert graph.cypher(
        "MATCH (c:Chunk {id: 'doc2-chunk2'})-[:HAS_ENTITY]->(e:Entity) RETURN e.id AS e ORDER BY e"
    ).to_list() == [{"e": "Charles Darwin::Person"}]
    assert graph.cypher(
        "MATCH (e:Entity)-[:BELONGS_TO]->(c:Cluster) RETURN c.topics AS topics, count(e) AS n ORDER BY n DESC, topics"
    ).to_list()[0] == {"topics": ["Person"], "n": 2}
    # The same ranking as the per-document load.
    per_document = module.run()
    assert [(r["source"], r["type"], r["target"]) for r in result["ranked"]] == [
        (r["source"], r["type"], r["target"]) for r in per_document["ranked"]
    ]


def test_a_single_per_document_dict_and_its_own_endpoint_types_are_read() -> None:
    module = _load_example()
    doc = copy.deepcopy(module.DOCS[1])
    for relation in doc["graph"]["relations"]:
        relation["source_type"], relation["target_type"] = (
            "Person",
            {"radium": "Element"}.get(relation["target"], "Person"),
        )
    graph = kglite.from_records(module.knwler_to_spec(doc), on_missing_endpoint="error")
    assert graph.cypher("MATCH (d:Document) RETURN d.id AS id").to_list() == [{"id": "doc2"}]
    assert graph.cypher("MATCH (:Entity {name: 'Marie Curie'})-[:read]->(t:Entity) RETURN t.id AS t").to_list() == [
        {"t": "Charles Darwin::Person"}
    ]
