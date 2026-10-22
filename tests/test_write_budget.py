"""The unsaved-write budget of a read-only mapped load.

A graph served from a file's mapped topology keeps its writes in heap memory
until a save writes them out. `write_budget=` bounds that heap: a warning at
half the limit, a refusal at the limit, and a save starts over. The Rust suite
covers the engine (rollback, transactions, declined rebase); these tests pin the
Python surface: the keyword, the warning category, the exception, `graph_info()`
and the exemptions.
"""

from __future__ import annotations

import shutil
import sys
import warnings

import numpy as np
import pandas as pd
import pytest

import kglite

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="Auto never maps on Windows")

NODES = 70_000  # above the 65,536-node floor, so a save records topology sections in any build
# The first write to a loaded graph costs ~9 bytes per node (630 KB at NODES), so the
# limit sits above that and below what the bulk statement adds.
TINY = 1 << 20
BULK = "UNWIND range(1, 4000) AS i CREATE (:Doc {id: 100000 + i, n: i})"


@pytest.fixture(scope="module")
def _base(tmp_path_factory):
    graph = kglite.KnowledgeGraph()
    ids = np.arange(1, NODES + 1)
    graph.add_nodes(pd.DataFrame({"id": ids, "title": [f"d{i}" for i in ids], "n": ids}), "Doc", "id", "title")
    ring = pd.DataFrame({"s": ids, "t": ids % NODES + 1})
    graph.add_relationships(ring, "LINKS", "Doc", "s", "Doc", "t")
    path = tmp_path_factory.mktemp("budget") / "g.kgl"
    graph.save(str(path))
    return path


@pytest.fixture
def saved(_base, tmp_path):
    path = tmp_path / "g.kgl"
    shutil.copy(_base, path)
    return str(path)


def _load(path, **options):
    return kglite.load(path, storage="mapped", **options)


def _count(graph) -> int:
    return graph.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list()[0]["c"]


def _create(graph, i):
    graph.cypher(f"CREATE (:Doc {{id: {1_000_000 + i}, n: 1}})")


def test_a_mapped_load_reports_its_budget(saved):
    info = _load(saved).graph_info()
    assert info["write_budget_bytes"] == 512 << 20
    assert info["write_budget_used_bytes"] == 0
    assert info["write_budget_state"] == "ok"
    assert _load(saved, write_budget=None).graph_info()["write_budget_state"] == "unlimited"
    assert _load(saved, write_budget=TINY).graph_info()["write_budget_bytes"] == TINY
    memory = kglite.load(saved, storage="memory").graph_info()
    assert memory["write_budget_state"] == "not_applicable"
    assert memory["write_budget_bytes"] is None


def test_the_keyword_takes_bytes_none_or_default(saved):
    assert _load(saved, write_budget="default").graph_info()["write_budget_bytes"] == 512 << 20
    for bad in ("big", -1, 1.5, True):
        with pytest.raises(kglite.ArgumentError):
            _load(saved, write_budget=bad)


def test_writes_warn_once_then_raise_and_a_save_starts_over(saved, tmp_path):
    graph = _load(saved, write_budget=TINY)
    for round_ in range(2):
        warned = 0
        accepted = 0
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            with pytest.raises(kglite.WriteBudgetExceededError) as refused:
                for i in range(100_000):
                    _create(graph, round_ * 100_000 + i)
                    accepted += 1
        warned = sum(issubclass(w.category, kglite.WriteBudgetWarning) for w in caught)
        assert warned == 1, f"round {round_}: one warning per crossing"
        assert accepted > 3
        assert refused.value.code == "WriteBudgetExceeded"
        assert "save" in str(refused.value).lower()
        assert graph.graph_info()["write_budget_state"] in {"warning", "exceeded"}
        before = _count(graph)
        with pytest.raises(kglite.WriteBudgetExceededError):
            graph.cypher(BULK)
        assert _count(graph) == before, "a refused statement changes nothing; reads work"

        graph.save(str(tmp_path / f"saved{round_}.kgl"))
        info = graph.graph_info()
        assert info["write_budget_state"] == "ok"
        assert info["write_budget_used_bytes"] == 0
        _create(graph, 999_000 + round_)  # writes resume


def test_a_statement_that_crosses_is_rolled_back_whole(saved):
    graph = _load(saved, write_budget=TINY)
    before = _count(graph)
    with pytest.raises(kglite.WriteBudgetExceededError):
        graph.cypher(BULK)
    assert _count(graph) == before
    assert graph.cypher("MATCH (d:Doc {id: 100001}) RETURN count(d) AS c").to_list()[0]["c"] == 0


def test_a_refused_statement_leaves_the_usage_where_it_was(saved):
    graph = _load(saved, write_budget=TINY)
    _create(graph, 1)
    used = graph.graph_info()["write_budget_used_bytes"]
    state = graph.graph_info()["write_budget_state"]
    for _ in range(25):
        with pytest.raises(kglite.WriteBudgetExceededError):
            graph.cypher(BULK)
        info = graph.graph_info()
        assert info["write_budget_used_bytes"] == used, "the rollback takes its overlay entries with it"
        assert info["write_budget_state"] == state
    _create(graph, 2)  # a statement that fits is still accepted


def test_failed_statements_do_not_grow_the_overlay(saved):
    graph = _load(saved, write_budget=None)
    _create(graph, 1)
    used = graph.graph_info()["topology_overlay_entries"]
    for _ in range(25):
        with pytest.raises(kglite.CypherExecutionError):
            graph.cypher("UNWIND range(1, 5) AS i CREATE (:Doc {id: 5000 + i, n: 10 / (3 - i)})")
    assert graph.graph_info()["topology_overlay_entries"] == used


def test_a_transaction_over_the_limit_refuses_and_rolls_back(saved):
    graph = _load(saved, write_budget=TINY)
    before = _count(graph)
    with graph.begin() as tx:
        with pytest.raises(kglite.WriteBudgetExceededError):
            tx.cypher(BULK)
    assert _count(graph) == before


def test_a_session_refuses_at_the_limit_and_keeps_reading(saved):
    session = kglite.open_session(saved, storage="mapped", write_budget=0)
    with pytest.raises(kglite.WriteBudgetExceededError):
        session.execute("CREATE (:Doc {id: 777777, n: 1})")
    assert session.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list()[0]["c"] == NODES


def test_a_fluent_loader_is_refused_when_the_graph_is_at_the_limit(saved):
    graph = _load(saved, write_budget=0)
    frame = pd.DataFrame({"id": [5001, 5002], "title": ["a", "b"]})
    with pytest.raises(kglite.WriteBudgetExceededError):
        graph.add_nodes(frame, "Doc", "id", "title")
    assert _count(graph) == NODES


def test_a_zero_budget_and_no_limit(saved):
    graph = _load(saved, write_budget=0)
    with pytest.raises(kglite.WriteBudgetExceededError):
        _create(graph, 1)
    free = _load(saved, write_budget=None)
    with warnings.catch_warnings():
        warnings.simplefilter("error", kglite.WriteBudgetWarning)
        free.cypher(BULK)
    assert free.graph_info()["write_budget_used_bytes"] > 0


def test_a_durable_graph_carries_no_budget(saved):
    graph = kglite.open(saved, durable="normal")
    try:
        graph.cypher(BULK)
        assert graph.graph_info()["write_budget_state"] == "not_applicable"
    finally:
        graph.close()


def test_a_durable_session_accepts_a_bulk_write(saved):
    session = kglite.open_session(saved, durable="normal")
    try:
        session.execute(BULK)
    finally:
        session.close()


def test_a_durable_session_rejects_the_keyword(saved):
    with pytest.raises(ValueError, match="write_budget"):
        kglite.open_session(saved, durable="normal", write_budget=1)


# --- Embedding writes count against the budget -----------------------------------------

EMB_DIM = 256
EMB_LIMIT = 4 << 20  # a 2000-vector store (2 MiB) reaches half of it


def _vectors(count: int, offset: int = 0) -> dict[int, list[float]]:
    return {i: [float((i + j) % 7) for j in range(EMB_DIM)] for i in range(1 + offset, count + 1 + offset)}


def _stores(graph) -> list[tuple[str, str, int]]:
    return sorted((s["node_type"], s["text_column"], s["count"]) for s in graph.list_embeddings())


class _ZeroEmbedder:
    dimension = EMB_DIM

    def embed(self, texts: list[str]) -> list[list[float]]:
        return [[0.5] * EMB_DIM for _ in texts]


def test_set_embeddings_warns_then_the_next_embedding_write_is_refused(saved, tmp_path):
    graph = _load(saved, write_budget=EMB_LIMIT)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        graph.set_embeddings("Doc", "title", _vectors(2000))
    assert sum(issubclass(w.category, kglite.WriteBudgetWarning) for w in caught) == 1
    info = graph.graph_info()
    assert info["write_budget_used_bytes"] >= 2000 * EMB_DIM * 4
    assert info["write_budget_state"] == "warning"

    graph.set_embeddings("Doc", "n", _vectors(2500))
    assert graph.graph_info()["write_budget_state"] == "exceeded"
    stores = _stores(graph)
    with pytest.raises(kglite.WriteBudgetExceededError) as refused:
        graph.set_embeddings("Doc", "id", _vectors(10))
    assert refused.value.code == "WriteBudgetExceeded"
    assert _stores(graph) == stores, "a refused embedding write changes nothing"
    with pytest.raises(kglite.WriteBudgetExceededError):
        _create(graph, 1)
    with pytest.raises(kglite.WriteBudgetExceededError):
        graph.quantize_embeddings("Doc", "title", "int8")
    assert _stores(graph) == stores

    graph.save(str(tmp_path / "released.kgl"))
    info = graph.graph_info()
    assert info["write_budget_state"] == "ok"
    assert info["write_budget_used_bytes"] == 0
    assert _stores(graph) == stores
    graph.set_embeddings("Doc", "id", _vectors(10))  # writes resume


def test_embed_texts_is_checked_before_and_after_it_runs(saved):
    graph = _load(saved, write_budget=EMB_LIMIT)
    graph.set_embedder(_ZeroEmbedder())
    with pytest.warns(kglite.WriteBudgetWarning):
        graph.embed_texts("Doc", "title", show_progress=False)  # 70,000 x 1 KiB: overshoots by its own size
    assert graph.graph_info()["write_budget_state"] == "exceeded"
    stores = _stores(graph)
    with pytest.raises(kglite.WriteBudgetExceededError):
        graph.embed_texts("Doc", "n", show_progress=False)
    assert _stores(graph) == stores


def test_import_embeddings_is_metered(saved, tmp_path):
    source = _load(saved, write_budget=None)
    source.set_embeddings("Doc", "title", _vectors(3000))
    export = str(tmp_path / "vectors.kgle")
    source.export_embeddings(export)

    graph = _load(saved, write_budget=EMB_LIMIT // 2)
    with pytest.warns(kglite.WriteBudgetWarning):
        graph.import_embeddings(export)
    assert graph.graph_info()["write_budget_state"] == "exceeded"
    with pytest.raises(kglite.WriteBudgetExceededError):
        graph.import_embeddings(export)


def test_memory_storage_and_unlimited_budgets_do_not_meter_embeddings(saved):
    memory = kglite.load(saved, storage="memory", write_budget=1)
    memory.set_embeddings("Doc", "title", _vectors(2000))
    memory.set_embeddings("Doc", "n", _vectors(2000))
    assert memory.graph_info()["write_budget_state"] == "not_applicable"

    free = _load(saved, write_budget=None)
    free.set_embeddings("Doc", "title", _vectors(2000))
    assert free.graph_info()["write_budget_state"] == "unlimited"


def test_a_save_keeps_the_columns_where_the_load_put_them(tmp_path):
    """The rebase after a save reloads the file the way the caller loaded it."""
    graph = kglite.KnowledgeGraph()
    ids = np.arange(1, NODES + 1)
    frame = pd.DataFrame({"id": ids, "title": [f"d{i}" for i in ids], "ratio": ids / 7.0})
    graph.add_nodes(frame, "Doc", "id", "title")
    graph.add_relationships(pd.DataFrame({"s": ids, "t": ids % NODES + 1}), "LINKS", "Doc", "s", "Doc", "t")
    path = str(tmp_path / "wide.kgl")
    graph.save(path)

    def heap_after_save(name, **options):
        loaded = _load(path, **options)
        loaded.cypher("CREATE (:Doc {id: 1000001, ratio: 1.0})")
        loaded.save(str(tmp_path / name))
        return loaded.graph_info()["columnar_heap_bytes"]

    mapped = heap_after_save("a.kgl")
    on_heap = heap_after_save("b.kgl", columns="memory")
    assert on_heap - mapped > 100_000, "columns='memory' is not turned into file mappings by the save"
