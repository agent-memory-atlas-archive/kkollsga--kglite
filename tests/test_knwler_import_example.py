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
