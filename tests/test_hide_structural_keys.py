"""`hide_structural_keys` through the Python wheel, on every storage mode.

Rust twin: `hidden_structural_keys_tests.rs`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from .test_value_variants import STORAGE_MODES, _new_graph


@pytest.fixture(params=STORAGE_MODES)
def graph(request, tmp_path: Path):
    return _new_graph(request.param, tmp_path)


def scalar(g, query, **kwargs):
    rows = list(g.cypher(query, **kwargs))
    assert len(rows) == 1, query
    return next(iter(rows[0].values()))


# ── hide_structural_keys ───────────────────────────────────────────────────


def test_hide_structural_keys_is_off_by_default_and_opt_in(graph):
    graph.cypher("CREATE (:J {id: 'database', v: 1}), (:T {id: 1, title: 'Hello', type: 'custom'})")
    assert scalar(graph, "MATCH (n:J) RETURN keys(n)") == ["id", "title", "type", "v"]
    assert scalar(graph, "MATCH (n:J) RETURN keys(n)", hide_structural_keys=True) == ["id", "v"]
    props = scalar(graph, "MATCH (n:J) RETURN properties(n)", hide_structural_keys=True)
    assert props == {"id": "database", "v": 1}
    assert scalar(graph, "MATCH (n:J) RETURN n {.*}", hide_structural_keys=True) == {"id": "database", "v": 1}
    # User data named title / type survives.
    assert scalar(graph, "MATCH (n:T) RETURN keys(n)", hide_structural_keys=True) == ["id", "title", "type"]
    assert scalar(graph, "MATCH (n:T) RETURN properties(n).title", hide_structural_keys=True) == "Hello"


def test_hide_structural_keys_leaves_return_n_alone(graph):
    graph.cypher("CREATE (:J {id: 'database', v: 1})")
    node = scalar(graph, "MATCH (n:J) RETURN n", hide_structural_keys=True)
    assert {"id", "title", "type", "v"} <= set(node["properties"])


def test_hide_structural_keys_on_a_mutation(graph):
    graph.cypher("CREATE (:J {id: 'database', v: 1})")
    keys = scalar(graph, "MATCH (n:J) SET n.v = 2 RETURN keys(n)", hide_structural_keys=True)
    assert keys == ["id", "v"]


def test_hide_structural_keys_relationships(graph):
    graph.cypher("CREATE (:A {id: 1})-[:R {rid: 5}]->(:B {id: 2})")
    assert scalar(graph, "MATCH ()-[r:R]->() RETURN keys(r)") == ["rid", "type"]
    assert scalar(graph, "MATCH ()-[r:R]->() RETURN keys(r)", hide_structural_keys=True) == ["rid"]
