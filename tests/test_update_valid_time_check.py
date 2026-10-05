"""The fluent `update()` and the `store_as=` writers judge the rows they leave
against the valid-time declarations, as Cypher `SET` does.

They wrote without judging, so a bound that is not a date, or an inverted
interval, reached the graph — and every `AS OF` read of the type then raised.
A refusal raises and writes nothing; all of `update()`'s properties are judged
together, so moving an interval by writing both bounds is one legal write.
"""

from __future__ import annotations

import datetime as dt
import warnings

import pandas as pd
import pytest

import kglite


@pytest.fixture
def declared():
    g = kglite.KnowledgeGraph()
    g.add_nodes(
        pd.DataFrame(
            {
                "id": [1, 2],
                "title": ["a", "b"],
                "n": [5, 6],
                "vf": [dt.date(2010, 1, 1), dt.date(2012, 1, 1)],
                "vt": [dt.date(2011, 1, 1), dt.date(2013, 1, 1)],
            }
        ),
        "T",
        "id",
        "title",
    )
    g.set_temporal("T", "vf", "vt")
    return g


def _bounds(g):
    return g.cypher("FOR VALID_TIME ALL MATCH (t:T) RETURN t.id AS id, t.vf AS vf, t.vt AS vt ORDER BY id").to_list()


@pytest.mark.parametrize(
    "values",
    [{"vt": dt.date(2000, 1, 1)}, {"vt": "not a date"}, {"vf": dt.date(2020, 1, 1)}],
    ids=["inverted", "unreadable", "from-past-to"],
)
def test_update_refuses_a_row_the_declaration_rejects(declared, values) -> None:
    selection = declared.select("T", temporal=False).where({"id": 1})
    before = _bounds(selection)
    with pytest.raises(kglite.ArgumentError, match="node '1'"):
        selection.update(values)
    assert _bounds(selection) == before
    assert _bounds(declared) == before


def test_update_judges_both_bounds_together(declared) -> None:
    moved = (
        declared.select("T", temporal=False)
        .where({"id": 1})
        .update({"vf": dt.date(2020, 1, 1), "vt": dt.date(2021, 1, 1)})["graph"]
    )
    assert _bounds(moved)[0] == {"id": 1, "vf": dt.date(2020, 1, 1), "vt": dt.date(2021, 1, 1)}
    # Properties no declaration binds are written as before.
    other = declared.select("T", temporal=False).update({"n": 1})["graph"]
    assert other.cypher("FOR VALID_TIME ALL MATCH (t:T) RETURN sum(t.n) AS s").to_list() == [{"s": 2}]


def test_store_as_writers_are_judged_too(declared) -> None:
    with pytest.raises(kglite.ArgumentError, match="property 'vt'"):
        declared.select("T", temporal=False).calculate("n * 2", store_as="vt")


def test_an_empty_interval_is_written_and_warned_about() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:S {id: 1, title: 's', vf: date('2010-01-01'), vt: date('2011-01-01')})")
    g.cypher("CALL db.temporal.declare({node: 'S', from: 'vf', to: 'vt', convention: 'half_open'})")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        out = g.select("S", temporal=False).update({"vt": dt.date(2010, 1, 1)})["graph"]
    assert any("empty" in str(w.message) for w in caught), [str(w.message) for w in caught]
    assert out.cypher("FOR VALID_TIME ALL MATCH (s:S) RETURN s.vt AS vt").to_list() == [{"vt": dt.date(2010, 1, 1)}]
