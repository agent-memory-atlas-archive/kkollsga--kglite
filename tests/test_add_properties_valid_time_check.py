"""The fluent `add_properties()` judges the rows it leaves against the
valid-time declarations, as `update()` and Cypher `SET` do.

It copied ancestor properties onto the leaf nodes without judging them, so a
bound that is not a date, or an inverted interval, reached a declared type —
and every `AS OF` read of that type then raised. A refusal raises
`ArgumentError` and writes nothing, in every storage mode and for both the
copy-list and the rename-map forms.
"""

from __future__ import annotations

import datetime as dt
import warnings

import pytest

import kglite


def _graph(storage, tmp_path):
    if storage == "disk":
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    elif storage == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:P {id: 1, title: 'p', vt: 'not a date', early: date('2000-01-01'), same: date('2010-01-01')})")
    g.cypher("CREATE (:C {id: 2, title: 'c', vf: date('2010-01-01'), vt: date('2011-01-01')})")
    g.cypher("MATCH (p:P), (c:C) CREATE (p)-[:HAS]->(c)")
    g.cypher("CALL db.temporal.declare({node: 'C', from: 'vf', to: 'vt', convention: 'half_open'})")
    return g


def _bounds(g):
    return g.cypher("FOR VALID_TIME ALL MATCH (c:C) RETURN c.vf AS vf, c.vt AS vt").to_list()


STORAGES = ["memory", "mapped", "disk"]
SPECS = [
    ({"P": ["vt"]}, "property 'vt'"),
    ({"P": {"vt": "vt"}}, "property 'vt'"),
    ({"P": {"vt": "early"}}, "node '2'"),
]


@pytest.mark.parametrize("storage", STORAGES)
@pytest.mark.parametrize(("spec", "match"), SPECS, ids=["copy-list-unreadable", "rename-unreadable", "rename-inverted"])
def test_add_properties_refuses_a_row_the_declaration_rejects(storage, tmp_path, spec, match) -> None:
    g = _graph(storage, tmp_path)
    selection = g.select("P").traverse("HAS", temporal=False)
    before = _bounds(selection)
    with pytest.raises(kglite.ArgumentError, match=match):
        selection.add_properties(spec)
    assert _bounds(selection) == before
    assert _bounds(g) == before


@pytest.mark.parametrize("storage", STORAGES)
def test_add_properties_writes_an_empty_interval_with_a_warning(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        out = g.select("P").traverse("HAS", temporal=False).add_properties({"P": {"vt": "same"}})
    messages = [str(w.message) for w in caught if issubclass(w.category, UserWarning)]
    assert any("empty interval" in m for m in messages), messages
    assert _bounds(out) == [{"vf": dt.date(2010, 1, 1), "vt": dt.date(2010, 1, 1)}]
