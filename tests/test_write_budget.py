"""The unsaved-write budget of a read-only mapped load.

A graph served from a file's mapped topology keeps its writes in heap memory
until a save writes them out. `write_budget=` bounds that heap: a warning at
half the limit, a refusal at the limit, and a save starts over. The Rust suite
covers the engine (rollback, transactions, declined rebase); these tests pin the
Python surface: the keyword, the warning category, the exception, `graph_info()`
and the exemptions.
"""

from __future__ import annotations

import sys
import warnings

import pytest

import kglite

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="Auto never maps on Windows")

TINY = 12_000
BULK = "UNWIND range(1, 4000) AS i CREATE (:Doc {id: 100000 + i, n: i})"


@pytest.fixture
def saved(tmp_path, monkeypatch):
    # Debug builds only: a tiny graph records the topology sections.
    monkeypatch.setenv("KGLITE_RAW_SECTION_FLOORS", "1,1")
    graph = kglite.KnowledgeGraph()
    graph.cypher("UNWIND range(1, 60) AS i CREATE (:Doc {id: i, n: i})")
    graph.cypher("MATCH (a:Doc), (b:Doc) WHERE b.id = a.id % 60 + 1 CREATE (a)-[:LINKS]->(b)")
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    return path


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
    for _ in range(25):
        with pytest.raises(kglite.WriteBudgetExceededError):
            graph.cypher(BULK)
        info = graph.graph_info()
        assert info["write_budget_used_bytes"] == used, "the rollback takes its overlay entries with it"
        assert info["write_budget_state"] == "ok"
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
    assert session.cypher("MATCH (d:Doc) RETURN count(d) AS c").to_list()[0]["c"] == 60


def test_a_fluent_loader_is_refused_when_the_graph_is_at_the_limit(saved):
    import pandas as pd

    graph = _load(saved, write_budget=0)
    frame = pd.DataFrame({"id": [5001, 5002], "title": ["a", "b"]})
    with pytest.raises(kglite.WriteBudgetExceededError):
        graph.add_nodes(frame, "Doc", "id", "title")
    assert _count(graph) == 60


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
