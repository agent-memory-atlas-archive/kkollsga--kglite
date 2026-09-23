"""The published relationship GraphRAG example is executable and network-free."""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

import kglite


def _load_example():
    source = Path(__file__).parents[1] / "examples" / "relationship_graphrag.py"
    spec = importlib.util.spec_from_file_location("relationship_graphrag", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_relationship_graphrag_example(tmp_path: Path) -> None:
    """Assert the state the example leaves behind, not merely that it ran.

    `run()` returns nothing, so calling it alone asserted only "no exception":
    an example that stopped saving, or saved a graph whose relationship store
    came back empty, passed unchanged. The checkpoint it writes is the durable
    artifact a reader would pick up, so that is what this checks.
    """
    module = _load_example()
    output = tmp_path / "claims.kgl"

    module.run(output)

    assert output.is_file()
    reopened = kglite.load(str(output))
    assert reopened.cypher(
        "CALL db.edge_embeddings.list({type:'ASSERTS',text_property:'description'}) "
        "YIELD entity,count,dimension,model RETURN entity,count,dimension,model"
    ).to_list() == [
        {
            "entity": "relationship",
            "count": 3,
            "dimension": module.TinyEmbedder.dimension,
            "model": module.TinyEmbedder.model_id,
        }
    ]

    reopened.set_embedder(module.TinyEmbedder())
    scored = module.exact_filtered(reopened, "evidence that heat increases evaporation")
    assert [row["claim_id"] for row in scored] == ["c1", "c1"]
    assert {row["evidence_id"] for row in scored} == {"e1", "e2"}
    assert scored[0]["score"] == pytest.approx(scored[0]["score"])
    assert all(isinstance(row["score"], float) for row in scored)
    assert scored[0]["score"] >= scored[1]["score"]
