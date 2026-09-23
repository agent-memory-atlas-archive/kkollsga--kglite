"""The published relationship GraphRAG example is executable and network-free."""

from __future__ import annotations

import importlib.util
from pathlib import Path


def test_relationship_graphrag_example(tmp_path: Path) -> None:
    source = Path(__file__).parents[1] / "examples" / "relationship_graphrag.py"
    spec = importlib.util.spec_from_file_location("relationship_graphrag", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.run(tmp_path / "claims.kgl")
