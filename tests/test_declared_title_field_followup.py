"""A follow-up `add_nodes` without `node_title_field` keeps the type's declared
title field.

A frame carrying a `title` column used to rebind the title to that column: the
node's title became the `title` value, and `n.name` (the declared spelling)
answered it while `properties(n).name` still held the old one. The declared
field stays the title; the frame's `title` column is stored as an ordinary
property (shadowed by the title, so unreadable), with a warning saying so.
"""

from __future__ import annotations

import warnings

import pandas as pd
import pytest

import kglite


def _graph(storage, tmp_path):
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def _read(g):
    return g.cypher(
        "MATCH (n:P) RETURN n.title AS title, n.name AS name, properties(n).name AS pname ORDER BY n.id"
    ).to_list()


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
@pytest.mark.parametrize("conflict", [None, "update"])
def test_a_follow_up_load_keeps_the_declared_title_field(storage, conflict, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    g.add_nodes(pd.DataFrame({"id": [1], "name": ["Bob"]}), "P", "id", node_title_field="name")
    frame = pd.DataFrame({"id": [1, 2], "name": ["Bob", "Ann"], "title": ["Dr", "Prof"]})
    kwargs = {} if conflict is None else {"conflict_handling": conflict}
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g.add_nodes(frame, "P", "id", **kwargs)
    messages = [str(w.message) for w in caught if issubclass(w.category, UserWarning)]
    assert any("stored as an ordinary property" in m and "'name'" in m for m in messages), messages
    rows = _read(g)
    assert [r["title"] for r in rows] == ["Bob", "Ann"]
    for r in rows:
        assert r["name"] == r["pname"] == r["title"]


def test_update_without_the_declared_column_leaves_titles_alone() -> None:
    g = kglite.KnowledgeGraph()
    g.add_nodes(pd.DataFrame({"id": [1], "name": ["Bob"]}), "P", "id", node_title_field="name")
    with warnings.catch_warnings(record=True):
        warnings.simplefilter("always")
        g.add_nodes(pd.DataFrame({"id": [1], "title": ["Dr"]}), "P", "id", conflict_handling="update")
    assert _read(g) == [{"title": "Bob", "name": "Bob", "pname": "Bob"}]
