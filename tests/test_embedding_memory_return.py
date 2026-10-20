"""Embedding import/remove hand their freed pages back to the OS.

mimalloc keeps a freed workload's pages until a collect. Measured on a 50k x
1024 store, `import_embeddings` left ~980 MB charged against ~226 MB once
trimmed, and `remove_embeddings` left 212 MB against 17 MB. The Python seam now
collects after every embedding store replacer/remover; these tests fail if it
stops, because the un-trimmed levels sit several store-sizes above the bounds.

No subprocess is started inside a measured window: a fork between the free and
the collect pins the pages.
"""

from __future__ import annotations

import gc

import pytest

import kglite
from tests.test_trim_memory import _footprint_bytes, requires_footprint

NODES = 20_000
DIM = 1024
STORE_BYTES = NODES * DIM * 4  # 82 MB of f32


@pytest.fixture
def graph_and_file(tmp_path):
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(1, %d) AS i CREATE (:Doc {id: i, text: 't'})" % NODES)
    row = [0.5] * DIM  # one shared list: keeps Python-side memory out of the measurement
    graph.set_embeddings("Doc", "text", {i: row for i in range(1, NODES + 1)})
    path = str(tmp_path / "docs.kgle")
    graph.export_embeddings(path)
    graph.remove_embeddings("Doc", "text")
    del row
    gc.collect()
    return graph, path


@requires_footprint
def test_import_and_remove_return_memory(graph_and_file):
    graph, path = graph_and_file
    kglite.trim_memory()
    level = _footprint_bytes()

    stats = graph.import_embeddings(path)
    assert stats["imported"] == NODES
    after_import = _footprint_bytes()
    # The live store is STORE_BYTES; the old behaviour held ~4-5x that.
    assert after_import - level <= int(STORE_BYTES * 1.6) + 40 * 1024 * 1024, (
        f"import left {after_import - level} bytes above the pre-import level (store is {STORE_BYTES})"
    )

    graph.remove_embeddings("Doc", "text")
    after_remove = _footprint_bytes()
    assert after_remove - level <= 30 * 1024 * 1024, (
        f"remove left {after_remove - level} bytes above the pre-import level (store was {STORE_BYTES})"
    )


def test_embedding_replacers_collect_at_their_seam(graph_and_file):
    graph, path = graph_and_file
    before = kglite._seam_trim_count()
    graph.import_embeddings(path)
    assert kglite._seam_trim_count() == before + 1
    graph.remove_embeddings("Doc", "text")
    assert kglite._seam_trim_count() == before + 2
    graph.set_embeddings("Doc", "text", {1: [0.5] * 4})
    assert kglite._seam_trim_count() == before + 3
    with pytest.raises(ValueError):
        graph.remove_embeddings("Doc", "missing")
    assert kglite._seam_trim_count() == before + 3


def test_exported_embedding_file_is_complete_and_leaves_no_temp(graph_and_file, tmp_path):
    _graph, path = graph_and_file
    assert sorted(p.name for p in tmp_path.iterdir()) == ["docs.kgle"]
