"""The `connection`-named loader and fluent methods are pointers to their
`relationship`-named twins: same signature, same result, same graph.

Each pair runs on twin graphs; the reports (minus timings), the error text and
the resulting edges must be identical.
"""

from __future__ import annotations

import inspect
import warnings

import pandas as pd
import pytest

from kglite import KnowledgeGraph

PAIRS = [
    ("add_connections", "add_relationships"),
    ("replace_connections", "replace_relationships"),
    ("add_connections_bulk", "add_relationships_bulk"),
    ("add_connections_from_source", "add_relationships_from_source"),
    ("connections", "relationships"),
    ("create_connections", "create_relationships"),
    ("connection_types", "relationship_types"),
]
TIMING_KEYS = {"processing_time_ms", "timestamp"}


@pytest.mark.parametrize(("pointer", "twin"), PAIRS)
def test_each_pointer_has_its_twins_signature(pointer: str, twin: str) -> None:
    assert inspect.signature(getattr(KnowledgeGraph, pointer)) == inspect.signature(getattr(KnowledgeGraph, twin))
    assert f":meth:`{twin}`" in (getattr(KnowledgeGraph, pointer).__doc__ or "") or (
        getattr(KnowledgeGraph, pointer).__doc__ or ""
    ).startswith(f"Pointer to {twin}()")


def _graph() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"id": [1, 2, 3], "name": ["A", "B", "C"]}), "Person", "id", "name")
    graph.add_nodes(pd.DataFrame({"id": [10, 11], "name": ["X", "Y"]}), "Company", "id", "name")
    return graph


def _edges(graph: KnowledgeGraph) -> list:
    rows = graph.cypher(
        "MATCH (a)-[r]->(b) RETURN a.id AS s, type(r) AS t, b.id AS d, properties(r) AS p ORDER BY s, t, d"
    ).to_list()
    return rows


def _scrub(value):
    if isinstance(value, dict):
        return {key: _scrub(item) for key, item in value.items() if key not in TIMING_KEYS}
    if isinstance(value, list):
        return [_scrub(item) for item in value]
    return value


def _run(graph: KnowledgeGraph, name: str, *args, **kwargs):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        try:
            result = getattr(graph, name)(*args, **kwargs)
            outcome = ("ok", _scrub(result) if not isinstance(result, KnowledgeGraph) else "graph")
        except Exception as error:  # noqa: BLE001 — the comparison is the point
            outcome = ("error", type(error).__name__, str(error))
    return outcome, [str(w.message) for w in caught], result if outcome[0] == "ok" else None


def _twin_run(pointer: str, twin: str, *args, prepare=None, **kwargs):
    left, right = _graph(), _graph()
    if prepare is not None:
        prepare(left)
        prepare(right)
    a = _run(left, pointer, *args, **kwargs)
    b = _run(right, twin, *args, **kwargs)
    assert a[:2] == b[:2]
    return a, b, left, right


WORKS = pd.DataFrame({"p": [1, 2, 3], "c": [10, 10, 11], "since": [2001, 2002, 2003]})
WORKS_WITH_NULL = pd.DataFrame({"p": [1, None], "c": [10, 11]})


@pytest.mark.parametrize(
    ("args", "kwargs"),
    [
        ((WORKS, "WORKS_AT", "Person", "p", "Company", "c"), {}),
        ((WORKS, "WORKS_AT", "Person", "p", "Company", "c"), {"columns": ["since"], "conflict_handling": "replace"}),
        ((WORKS_WITH_NULL, "WORKS_AT", "Person", "p", "Company", "c"), {}),  # the skip warning
        ((WORKS_WITH_NULL, "WORKS_AT", "Person", "p", "Company", "c"), {"on_invalid": "error"}),
        (
            (None, "SAME", "Person", "a", "Person", "b"),
            {"query": "MATCH (a:Person), (b:Person) WHERE a.id < b.id RETURN a.id AS a, b.id AS b"},
        ),
        ((WORKS, "WORKS_AT", "Person", "p", "Company", "c"), {"query": "MATCH (n) RETURN n"}),  # both modes
    ],
)
@pytest.mark.parametrize(("pointer", "twin"), [PAIRS[0], PAIRS[1]])
def test_frame_and_query_loaders_equal_their_twins(pointer: str, twin: str, args, kwargs) -> None:
    (outcome, warned, _), _, left, right = _twin_run(pointer, twin, *args, **kwargs)
    assert _edges(left) == _edges(right)
    for text in warned:
        assert "add_connections" not in text


def test_replace_prunes_the_same_edges_through_either_name() -> None:
    def seed(graph: KnowledgeGraph) -> None:
        graph.add_relationships(WORKS, "WORKS_AT", "Person", "p", "Company", "c")

    resync = pd.DataFrame({"p": [1], "c": [11]})
    _, _, left, right = _twin_run(
        "replace_connections", "replace_relationships", resync, "WORKS_AT", "Person", "p", "Company", "c", prepare=seed
    )
    assert _edges(left) == _edges(right)
    assert [(row["s"], row["d"]) for row in _edges(left)] == [(1, 11), (2, 10), (3, 11)]


def _specs():
    return [
        {
            "source_type": "Person",
            "target_type": "Company",
            "connection_name": "WORKS_AT",
            "data": pd.DataFrame({"source_id": [1, 2], "target_id": [10, 11]}),
        },
        {
            "source_type": "Person",
            "target_type": "Ghost",
            "connection_name": "HAUNTS",
            "data": pd.DataFrame({"source_id": [1], "target_id": [99]}),
        },
    ]


@pytest.mark.parametrize(("pointer", "twin"), [PAIRS[2], PAIRS[3]])
def test_bulk_loaders_equal_their_twins(pointer: str, twin: str) -> None:
    (outcome, _, _), _, left, right = _twin_run(pointer, twin, _specs(), git_sha="abc", modified_by="me")
    assert _edges(left) == _edges(right)
    bad = [
        {"source_type": "Person", "target_type": "Company", "data": pd.DataFrame({"source_id": [1], "target_id": [10]})}
    ]
    (refused, _, _), _, _, _ = _twin_run(pointer, twin, bad)
    assert refused == ("error", "KeyError", "\"Missing 'connection_name' in relationship spec\"")


def test_introspection_pointers_equal_their_twins() -> None:
    graph = _graph()
    graph.add_relationships(WORKS, "WORKS_AT", "Person", "p", "Company", "c")
    assert graph.connection_types() == graph.relationship_types()
    assert graph.relationship_types()[0]["type"] == "WORKS_AT"
    view = graph.select("Person")
    assert view.connections() == view.relationships()
    assert view.connections(include_node_properties=False, flatten_single_parent=False) == view.relationships(
        include_node_properties=False, flatten_single_parent=False
    )


def test_create_connections_equals_create_relationships() -> None:
    def seed(graph: KnowledgeGraph) -> None:
        graph.add_relationships(WORKS, "WORKS_AT", "Person", "p", "Company", "c")

    left, right = _graph(), _graph()
    seed(left)
    seed(right)
    with pytest.warns(UserWarning, match="chained graph view"):
        left = left.select("Person").traverse("WORKS_AT").create_connections("EMPLOYED_BY")
    with pytest.warns(UserWarning, match="chained graph view"):
        right = right.select("Person").traverse("WORKS_AT").create_relationships("EMPLOYED_BY")
    assert _edges(left) == _edges(right)
    assert any(row["t"] == "EMPLOYED_BY" for row in _edges(left))

    # The chained-view warning names the relationship spelling through either name.
    for name in ("create_connections", "create_relationships"):
        graph = _graph()
        seed(graph)
        with pytest.warns(UserWarning, match=r"^create_relationships\('X'\) was called on a chained graph view"):
            chained = graph.select("Person").traverse("WORKS_AT")
            getattr(chained, name)("X")
