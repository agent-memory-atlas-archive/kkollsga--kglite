"""A node has one id and one title, so a second spelling of either must not be
silently dropped.

`CREATE` refuses a declared title field and `title` that disagree, as it
already refuses two different id spellings; without a declared title field,
`title` is the title and `name` stays a readable property. `add_nodes` titles nodes from a `title` column when no
title field is named, and warns when an `id` / `title` column is shadowed by an
identity field taken from another column.
"""

from __future__ import annotations

import warnings

import pandas as pd
import pytest

import kglite


def _rows(graph, query):
    return graph.cypher(query).to_list()


@pytest.mark.parametrize("verb", ["CREATE", "MERGE"])
def test_title_is_the_title_and_name_stays_readable(verb) -> None:
    graph = kglite.KnowledgeGraph()
    graph.cypher(f"{verb} (:Q {{id: 1, title: 'Ann', name: 'Nan'}})")
    # A null spelling supplies nothing: the other one is the title. (MERGE
    # refuses a null key outright.)
    graph.cypher("CREATE (:Q {id: 2, name: null, title: 'Bea'})")
    graph.cypher(f"{verb} (:Q {{id: 3, name: 'Cid'}})")
    assert _rows(graph, "MATCH (n:Q) RETURN n.id AS id, n.title AS t ORDER BY id") == [
        {"id": 1, "t": "Ann"},
        {"id": 2, "t": "Bea"},
        {"id": 3, "t": "Cid"},
    ]
    assert _rows(graph, "MATCH (n:Q {id: 1}) RETURN n.name AS n") == [{"n": "Nan"}]


@pytest.mark.parametrize("verb", ["CREATE", "MERGE"])
def test_a_declared_title_field_and_a_different_title_are_refused(verb) -> None:
    graph = kglite.KnowledgeGraph()
    graph.add_nodes(pd.DataFrame({"id": [1], "label": ["L1"]}), "T", "id", "label")
    with pytest.raises(Exception, match="two different titles"):
        graph.cypher(f"{verb} (:T {{id: 2, label: 'L2', title: 'Ann2'}})")
    graph.cypher(f"{verb} (:T {{id: 3, label: 'L3', title: 'L3'}})")
    # A declared title field makes `name` an ordinary property, not a title.
    graph.cypher(f"{verb} (:T {{id: 4, label: 'L4', name: 'other'}})")
    assert _rows(graph, "MATCH (n:T) RETURN n.id AS id, n.title AS t, n.name AS n ORDER BY id") == [
        {"id": 1, "t": "L1", "n": "L1"},
        {"id": 3, "t": "L3", "n": "L3"},
        {"id": 4, "t": "L4", "n": "other"},
    ]


def test_add_nodes_titles_from_a_title_column_by_default() -> None:
    graph = kglite.KnowledgeGraph()
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        graph.add_nodes(pd.DataFrame({"id": [1], "name": ["Nan"], "title": ["Ann"]}), "S", "id")
    assert _rows(graph, "MATCH (n:S) RETURN n.title AS t, n.name AS n") == [{"t": "Ann", "n": "Nan"}]
    # No title column: the id stays the title, as before.
    graph.add_nodes(pd.DataFrame({"id": [1], "name": ["Nan"]}), "U", "id")
    assert _rows(graph, "MATCH (n:U) RETURN n.title AS t") == [{"t": 1}]


@pytest.mark.parametrize(
    ("frame", "id_field", "title_field", "shadowed"),
    [
        ({"id": [1], "name": ["Nan"], "title": ["Ann"]}, "id", "name", "title"),
        ({"id": [1], "label": ["Lab"], "title": ["Ann"]}, "id", "label", "title"),
        ({"id": [1], "title": ["Ann"]}, "id", "id", "title"),
        ({"pid": [10], "id": [99], "title": ["T"]}, "pid", "title", "id"),
    ],
)
def test_add_nodes_warns_when_an_identity_column_is_shadowed(frame, id_field, title_field, shadowed) -> None:
    graph = kglite.KnowledgeGraph()
    with pytest.warns(UserWarning, match=f"its '{shadowed}' column is not readable"):
        graph.add_nodes(pd.DataFrame(frame), "P", id_field, title_field)


@pytest.mark.parametrize(
    ("frame", "id_field", "title_field", "query", "value"),
    [
        # `n.title` reads the `id` column, the title source.
        ({"pid": [10], "id": ["x"]}, "pid", "id", "MATCH (n:P) RETURN n.title AS v", "x"),
        # `n.title` is the id's declared spelling, so it reads the `title` column.
        ({"title": ["x"], "name": ["Nan"]}, "title", "name", "MATCH (n:P) RETURN n.title AS v", "x"),
    ],
    ids=["id-column-is-the-title", "title-column-is-the-id"],
)
def test_no_shadow_warning_for_the_column_the_other_identity_reads(frame, id_field, title_field, query, value) -> None:
    """An `id` column that is the title source (or a `title` column that is
    the id source) is readable — through the other identity field — so the
    load must not warn that it is not."""
    graph = kglite.KnowledgeGraph()
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        graph.add_nodes(pd.DataFrame(frame), "P", id_field, title_field)
    messages = [str(w.message) for w in caught if "not readable" in str(w.message)]
    assert messages == []
    assert _rows(graph, query) == [{"v": value}]
