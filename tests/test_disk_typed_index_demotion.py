"""A typed property index answers like a scan after a SET demotes its column.

`SET n.tag = 18` on one row turns a string column into a mixed-kind one. The
persistent index over that property must keep indexing the string cells, in
every lookup form, on the same handle, after a save and after a reopen.
"""

from __future__ import annotations

import datetime as dt

import pandas as pd
import pytest

import kglite

DEMOTERS = {
    "int": "18",
    "list": "[1, 2]",
    "timestamp": "datetime('2024-05-06T07:08:09')",
    "float": "2.5",
    "bool": "true",
}


def _frame():
    ids = list(range(1, 10))
    return pd.DataFrame({"id": ids, "title": [f"Person-{i}" for i in ids], "tag": [f"t{i % 3}" for i in ids]})


def _ids(graph, query, params=None):
    return [r["id"] for r in graph.cypher(query, params=params).to_list()]


def _check(graph, expect):
    for tag, ids in expect.items():
        forms = [
            _ids(graph, f"MATCH (n:Person {{tag: '{tag}'}}) RETURN n.id AS id ORDER BY id"),
            _ids(graph, "MATCH (n:Person {tag: $t}) RETURN n.id AS id ORDER BY id", {"t": tag}),
            _ids(graph, "MATCH (n:Person) WHERE n.tag = $t RETURN n.id AS id ORDER BY id", {"t": tag}),
        ]
        for got in forms:
            assert got == ids, (tag, forms)
    prefix = _ids(graph, "MATCH (n:Person) WHERE n.tag STARTS WITH 't' RETURN n.id AS id ORDER BY id")
    assert prefix == sorted(i for ids in expect.values() for i in ids)


@pytest.mark.parametrize("kind", DEMOTERS)
@pytest.mark.parametrize("indexed", [True, False])
def test_index_matches_scan_after_a_demoting_set(tmp_path, kind, indexed):
    path = str(tmp_path / "g")
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.add_nodes(_frame(), "Person", "id", "title")
    if indexed:
        g.create_index("Person", "tag")
    g.cypher(f"MATCH (n:Person {{id: 5}}) SET n.tag = {DEMOTERS[kind]}")
    expect = {"t0": [3, 6, 9], "t1": [1, 4, 7], "t2": [2, 8]}
    _check(g, expect)
    g.save(path)
    _check(g, expect)
    del g
    g = kglite.load(path)
    _check(g, expect)
    # A second write to the demoted column keeps the index current.
    g.cypher("MATCH (n:Person {id: 3}) SET n.tag = 't2'")
    expect = {"t0": [6, 9], "t1": [1, 4, 7], "t2": [2, 3, 8]}
    _check(g, expect)
    g.save(path)
    del g
    _check(kglite.load(path), expect)
