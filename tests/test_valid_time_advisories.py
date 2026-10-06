"""Every advisory a write earns reaches the caller, and a refused fluent
`update()` writes nothing in any storage mode.

A labelled `add_nodes` replaced the load's whole warning list with its widened
empty-interval warning, so a shadowed identity column it also earned was never
raised. The declaration paths report each advisory once.
"""

from __future__ import annotations

import datetime as dt
import warnings

import pandas as pd
import pytest

import kglite

# id 1 ends on the day id 2 begins (abutting under closed); id 3 ends the day
# before it begins (an empty interval under empty_when='to_before_from').
ROWS = [
    (1, "2000-01-01", "2005-01-01"),
    (2, "2005-01-01", None),
    (3, "2010-01-02", "2010-01-01"),
]


def _user_warnings(caught):
    return [str(w.message) for w in caught if issubclass(w.category, UserWarning)]


def _frame():
    return pd.DataFrame(
        {
            "id": [r[0] for r in ROWS],
            "vf": [dt.date.fromisoformat(r[1]) for r in ROWS],
            "vt": [dt.date.fromisoformat(r[2]) if r[2] else None for r in ROWS],
        }
    )


def _assert_abut_and_empty(messages):
    assert len(messages) == 2, messages
    assert any("end on the day" in m for m in messages), messages
    assert any("empty interval" in m for m in messages), messages


def test_set_temporal_raises_both_advisories() -> None:
    g = kglite.KnowledgeGraph()
    g.add_nodes(_frame(), "S", "id")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g.set_temporal("S", "vf", "vt", convention="closed", empty_when="to_before_from")
    _assert_abut_and_empty(_user_warnings(caught))


def test_a_loader_declaration_raises_both_advisories() -> None:
    g = kglite.KnowledgeGraph()
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g.add_nodes(
            _frame(),
            "S",
            "id",
            column_types={"vf": "validFrom", "vt": "validTo"},
            convention="closed",
            empty_when="to_before_from",
        )
    _assert_abut_and_empty(_user_warnings(caught))


def test_a_cypher_declaration_reports_both_advisories() -> None:
    g = kglite.KnowledgeGraph()
    g.add_nodes(_frame(), "S", "id")
    res = g.cypher(
        "CALL db.temporal.declare({node: 'S', from: 'vf', to: 'vt', convention: 'closed', "
        "empty_when: 'to_before_from'}) YIELD declared RETURN declared"
    )
    _assert_abut_and_empty(list(res.warnings))


def test_a_labelled_load_raises_its_shadow_warning_beside_the_empty_interval() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:X {id: 1, title: 'x', vf: date('2000-01-01'), vt: date('2001-01-01')})")
    g.cypher("CALL db.temporal.declare({node: 'X', from: 'vf', to: 'vt', convention: 'half_open'})")
    day = dt.date(2020, 1, 1)
    frame = pd.DataFrame({"pid": [10], "id": [99], "vf": [day], "vt": [day]})
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g.add_nodes(frame, "P", "pid", labels=["X"])
    messages = _user_warnings(caught)
    assert any("empty interval" in m for m in messages), messages
    assert any("'id' column is not readable" in m for m in messages), messages


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
def test_a_refused_update_writes_no_node(storage, tmp_path) -> None:
    """Two nodes: one would be left valid, one inverted. The whole update is
    refused, so neither is written."""
    if storage == "disk":
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    elif storage == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:T {id: 1, title: 'a', vf: date('2010-01-01'), vt: date('2011-01-01')})")
    g.cypher("CREATE (:T {id: 2, title: 'b', vf: date('2012-01-01'), vt: date('2013-01-01')})")
    g.set_temporal("T", "vf", "vt")
    selection = g.select("T", temporal=False)
    query = "FOR VALID_TIME ALL MATCH (t:T) RETURN t.id AS id, t.vt AS vt ORDER BY id"
    before = selection.cypher(query).to_list()
    with pytest.raises(kglite.ArgumentError, match="node '2'"):
        selection.update({"vt": dt.date(2011, 6, 1)})
    assert selection.cypher(query).to_list() == before
    assert g.cypher(query).to_list() == before
