"""Literal, parameter and scan lookups of a title agree on a disk graph after appends.

A disk graph appends nodes to a tail; once the tail's schema names a property
`title` the global title index must still be built from the title column, or the
literal form `{title: 'x'}` answers nothing while `{title: $t}` still finds the
row.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite

FIELDS = ["title", "name", "label"]
SHAPES = ["tail_append", "create", "set_title"]


def _frame(field, ids):
    return pd.DataFrame({"id": ids, field: [f"Person-{i}" for i in ids], "score": [i * 10 for i in ids]})


def _ids(graph, query, params=None):
    return sorted(r["id"] for r in graph.cypher(query, params=params).to_list())


def _assert_lookups_agree(graph, expect):
    for text, ids in expect.items():
        scan = sorted(
            r["id"]
            for r in graph.cypher("MATCH (n:Person) RETURN n.id AS id, n.title AS t").to_list()
            if r["t"] == text
        )
        assert scan == ids, (text, "scan", scan)
        literal = _ids(graph, f"MATCH (n {{title: '{text}'}}) RETURN n.id AS id")
        typed = _ids(graph, f"MATCH (n:Person {{title: '{text}'}}) RETURN n.id AS id")
        param = _ids(graph, "MATCH (n {title: $t}) RETURN n.id AS id", {"t": text})
        where = _ids(graph, "MATCH (n:Person) WHERE n.title = $t RETURN n.id AS id", {"t": text})
        assert literal == typed == param == where == ids, (text, literal, typed, param, where)


@pytest.mark.parametrize("shape", SHAPES)
@pytest.mark.parametrize("field", FIELDS)
def test_title_lookup_forms_agree_after_save_and_reopen(tmp_path, field, shape):
    path = str(tmp_path / "g")
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.add_nodes(_frame(field, list(range(1, 6))), "Person", "id", field)
    g.save(path)
    del g
    g = kglite.load(path)
    expect = {"Person-1": [1], "Person-5": [5]}
    if shape == "tail_append":
        g.add_nodes(_frame(field, [100, 101]), "Person", "id", field)
        expect.update({"Person-100": [100], "Person-101": [101]})
    elif shape == "create":
        g.cypher("CREATE (n:Person {id: 100, title: 'Person-100'})")
        expect["Person-100"] = [100]
    else:
        g.add_nodes(_frame(field, [100]), "Person", "id", field)
        g.cypher("MATCH (n:Person {id: 100}) SET n.title = 'renamed'")
        expect["renamed"] = [100]
        expect["Person-100"] = []
    _assert_lookups_agree(g, expect)
    g.save(path)
    _assert_lookups_agree(g, expect)
    del g
    g = kglite.load(path)
    _assert_lookups_agree(g, expect)
    g.cypher("MATCH (n:Person {id: 2}) SET n.title = 'second'")
    expect["second"] = [2]
    expect["Person-2"] = []
    _assert_lookups_agree(g, expect)
    g.save(path)
    del g
    _assert_lookups_agree(kglite.load(path), expect)


def test_title_set_on_a_created_node_is_what_the_index_answers(tmp_path):
    """`CREATE` leaves a `title` property column that `SET n.title` does not touch."""
    path = str(tmp_path / "g")
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.cypher("CREATE (n:Person {id: 16, title: 'Person-16'})")
    g.cypher("CREATE (n:Person {id: 17, title: 'Person-17'})")
    g.cypher("MATCH (n:Person {id: 16}) SET n.title = 'renamed'")
    expect = {"renamed": [16], "Person-16": [], "Person-17": [17]}
    g.save(path)
    _assert_lookups_agree(g, expect)
    del g
    g = kglite.load(path)
    _assert_lookups_agree(g, expect)
    g.cypher("MATCH (n:Person {id: 17}) SET n.title = 'again'")
    expect.update({"again": [17], "Person-17": []})
    g.save(path)
    _assert_lookups_agree(g, expect)
    del g
    _assert_lookups_agree(kglite.load(path), expect)
