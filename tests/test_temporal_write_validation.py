"""A write onto a declared type answers to the declaration's row rule: an
inverted interval, an empty one under ``half_open`` (from = to) and a bound
that is not a date are refused by ``add_nodes``, ``add_relationships`` and
Cypher ``CREATE`` / ``MERGE`` / ``SET`` with the declaration's own wording,
naming the load row or the element. NULL bounds stay open.

Red proof: before the check every refused write below was accepted, and
``db.temporal.declarations()`` counted it afterwards in ``empty_rows`` /
``unreadable_rows``.
"""

from __future__ import annotations

import json
import warnings

import pandas as pd
import pytest

import kglite
from kglite.blueprint import from_blueprint

MODES = [None, "mapped", "disk"]
MODE_IDS = ["memory", "mapped", "disk"]

COUNTS = "CALL db.temporal.declarations() YIELD empty_rows, unreadable_rows RETURN empty_rows, unreadable_rows"


def _graph(storage, tmp_path, convention="half_open") -> kglite.KnowledgeGraph:
    if storage == "disk":
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    elif storage == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (:Status {id: 1, vf: date('2000-01-01'), vt: date('2005-01-01')}), "
        "(:Status {id: 2, vf: date('2005-01-01'), vt: null}), "
        "(:Co {id: 10}), (:Co {id: 11})"
    ).to_list()
    g.cypher(
        "MATCH (s:Status {id: 1}), (c:Co {id: 10}) "
        "CREATE (s)-[:OP {vf: date('2000-01-01'), vt: date('2001-01-01')}]->(c)"
    ).to_list()
    g.cypher(
        f"CALL db.temporal.declare({{node: 'Status', from: 'vf', to: 'vt', convention: '{convention}'}})"
    ).to_list()
    g.cypher(
        f"CALL db.temporal.declare({{relationship: 'OP', from: 'vf', to: 'vt', convention: '{convention}'}})"
    ).to_list()
    return g


def _statuses(g) -> list:
    return g.cypher("MATCH (s:Status) RETURN s.id AS id, s.vf AS vf, s.vt AS vt ORDER BY id").to_list()


def _ops(g) -> int:
    return g.cypher("MATCH ()-[r:OP]->() RETURN count(r) AS n").to_list()[0]["n"]


def _clean(g) -> None:
    assert g.cypher(COUNTS).to_list() == [
        {"empty_rows": 0, "unreadable_rows": 0},
        {"empty_rows": 0, "unreadable_rows": 0},
    ]


def _frame(rows):
    return pd.DataFrame(
        {
            "id": [r[0] for r in rows],
            "vf": pd.to_datetime([r[1] for r in rows]),
            "vt": pd.to_datetime([r[2] for r in rows]),
        }
    )


# ── Loaders ──────────────────────────────────────────────────────────────


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_add_nodes_refuses_an_inverted_or_empty_row_naming_it(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    before = _statuses(g)
    inverted = _frame([(3, "2010-01-01", None), (4, "2012-01-01", "2011-01-01")])
    with pytest.raises(kglite.ArgumentError, match=r"row 1 \(0-based\) of the load, .*is after the to bound"):
        g.add_nodes(inverted, "Status", "id")
    empty = _frame([(5, "2013-01-01", "2013-01-01")])
    with pytest.raises(
        kglite.ArgumentError, match=r"row 0 \(0-based\) of the load, .*equals the to bound.*'half_open'"
    ):
        g.add_nodes(empty, "Status", "id")
    assert _statuses(g) == before
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_add_nodes_judges_an_update_by_the_bound_it_keeps(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    # Only `vt` arrives; the stored `vf` (2000-01-01) is after it.
    update = pd.DataFrame({"id": [1], "vt": pd.to_datetime(["1999-01-01"])})
    with pytest.raises(kglite.ArgumentError, match="is after the to bound"):
        g.add_nodes(update, "Status", "id")
    g.add_nodes(update, "Status", "id", conflict_handling="skip")
    assert _statuses(g)[0]["vt"].isoformat() == "2005-01-01"


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_add_nodes_accepts_null_bounds(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    frame = pd.DataFrame({"id": [3, 4], "vf": pd.to_datetime([None, "2010-01-01"]), "vt": pd.to_datetime([None, None])})
    g.add_nodes(frame, "Status", "id")
    assert [r["id"] for r in _statuses(g)] == [1, 2, 3, 4]
    _clean(g)


def test_add_nodes_refuses_an_unreadable_bound() -> None:
    g = _graph(None, None)
    frame = pd.DataFrame({"id": [3], "vf": ["someday"], "vt": [None]})
    with pytest.raises(kglite.ArgumentError, match=r"row 0 \(0-based\) of the load, property 'vf'.*someday"):
        g.add_nodes(frame, "Status", "id")


def test_the_reproducer_second_load_is_refused() -> None:
    """User test 4 §8 B3: a column-typed load onto the declared type."""
    df = pd.DataFrame(
        {"id": ["a.1", "a.3"], "ident": ["a", "a"], "begin": ["2020-01-01", "2021-01-01"], "eind": ["2021-01-01", None]}
    )
    g = kglite.KnowledgeGraph()
    types = {"begin": "validFrom", "eind": "validTo"}
    g.add_nodes(df, "P", "id", "ident", column_types=types, convention="half_open")
    row = pd.DataFrame({"id": ["a.2"], "ident": ["a"], "begin": ["2021-01-01"], "eind": ["2021-01-01"]})
    with pytest.raises(kglite.ArgumentError, match=r"row 0 \(0-based\) of the load, .*an empty interval"):
        g.add_nodes(row, "P", "id", "ident", column_types=types, convention="half_open")
    assert g.cypher("MATCH (p:P) RETURN count(p) AS n").to_list() == [{"n": 2}]


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_add_relationships_refuses_an_inverted_or_empty_row(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    frame = pd.DataFrame(
        {
            "s": [1, 2],
            "t": [10, 11],
            "vf": pd.to_datetime(["2002-01-01", "2006-01-01"]),
            "vt": pd.to_datetime(["2003-01-01", "2006-01-01"]),
        }
    )
    with pytest.raises(kglite.ArgumentError, match=r"row 1 \(0-based\) of the load, .*equals the to bound"):
        g.add_relationships(frame, "OP", "Status", "s", "Co", "t")
    assert _ops(g) == 1
    frame.loc[1, "vt"] = pd.NaT
    g.add_relationships(frame, "OP", "Status", "s", "Co", "t")
    assert _ops(g) == 3
    _clean(g)


def test_a_blueprint_row_is_refused(tmp_path) -> None:
    pd.DataFrame({"sid": [1, 2], "vf": ["2000-01-01", "2005-01-01"], "vt": ["2001-01-01", "2004-01-01"]}).to_csv(
        tmp_path / "status.csv", index=False
    )
    bp = {
        "settings": {"root": str(tmp_path)},
        "nodes": {
            "Status": {
                "csv": "status.csv",
                "pk": "sid",
                "properties": {"vf": "date", "vt": "date"},
                "temporal": {"from": "vf", "to": "vt", "convention": "closed"},
            }
        },
    }
    path = tmp_path / "blueprint.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        with pytest.raises(Exception, match="node '2'.*is after the to bound"):
            from_blueprint(path, save=False)


# ── Cypher ───────────────────────────────────────────────────────────────


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_create_refuses_an_inverted_or_empty_node(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=r"node '3', the from bound .*is after the to bound"):
        g.cypher("CREATE (:Status {id: 3, vf: date('2010-01-01'), vt: date('2009-01-01')})")
    with pytest.raises(
        kglite.CypherExecutionError, match=r"node '3', .*an empty interval under convention 'half_open'"
    ):
        g.cypher("CREATE (:Status {id: 3, vf: date('2010-01-01'), vt: date('2010-01-01')})")
    with pytest.raises(kglite.CypherExecutionError, match=r"node '3', property 'vt'.*someday"):
        g.cypher("CREATE (:Status {id: 3, vf: date('2010-01-01'), vt: 'someday'})")
    assert len(_statuses(g)) == 2
    g.cypher("CREATE (:Status {id: 3, vf: null, vt: date('2010-01-01')})")
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_closed_accepts_from_equal_to(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path, convention="closed")
    g.cypher("CREATE (:Status {id: 3, vf: date('2010-01-01'), vt: date('2010-01-01')})")
    g.add_nodes(_frame([(4, "2011-01-01", "2011-01-01")]), "Status", "id")
    assert len(_statuses(g)) == 4
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_a_failed_unwind_create_rolls_back(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match="node '5'"):
        g.cypher(
            "UNWIND [{id: 3, vf: '2010-01-01', vt: '2011-01-01'}, {id: 4, vf: '2010-01-01', vt: null}, "
            "{id: 5, vf: '2012-01-01', vt: '2011-01-01'}] AS r "
            "CREATE (:Status {id: r.id, vf: date(r.vf), vt: CASE WHEN r.vt IS NULL THEN null ELSE date(r.vt) END})"
        )
    assert len(_statuses(g)) == 2


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_merge_refuses_on_create_and_on_match(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match="node '3'.*is after the to bound"):
        g.cypher("MERGE (s:Status {id: 3, vf: date('2010-01-01'), vt: date('2009-01-01')})")
    with pytest.raises(kglite.CypherExecutionError, match="node '3'.*is after the to bound"):
        g.cypher("MERGE (s:Status {id: 3}) ON CREATE SET s.vf = date('2010-01-01'), s.vt = date('2009-01-01')")
    with pytest.raises(kglite.CypherExecutionError, match="node '1'.*equals the to bound"):
        g.cypher("MERGE (s:Status {id: 1}) ON MATCH SET s.vt = date('2000-01-01')")
    assert len(_statuses(g)) == 2
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_set_refuses_and_rolls_back(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    before = _statuses(g)
    with pytest.raises(kglite.CypherExecutionError, match="node '1', the from bound 2000-01-01.*is after the to bound"):
        g.cypher("MATCH (s:Status) SET s.vt = date('1999-01-01')")
    with pytest.raises(kglite.CypherExecutionError, match="node '1'.*equals the to bound"):
        g.cypher("MATCH (s:Status {id: 1}) SET s += {vt: date('2000-01-01')}")
    with pytest.raises(kglite.CypherExecutionError, match="node '2', property 'vf'"):
        g.cypher("MATCH (s:Status {id: 2}) SET s = {vf: 2005}")
    assert _statuses(g) == before
    # Judged on the interval the clause leaves, not item by item.
    g.cypher("MATCH (s:Status {id: 1}) SET s.vf = date('2020-01-01'), s.vt = date('2021-01-01')")
    g.cypher("MATCH (s:Status {id: 1}) SET s.vt = null")
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_relationship_create_and_set_are_refused(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"OP relationship from node '2' to node '11', the from bound .*is after the to bound",
    ):
        g.cypher(
            "MATCH (s:Status {id: 2}), (c:Co {id: 11}) "
            "CREATE (s)-[:OP {vf: date('2010-01-01'), vt: date('2009-01-01')}]->(c)"
        )
    with pytest.raises(kglite.CypherExecutionError, match="OP relationship from node '1' to node '10'"):
        g.cypher("MATCH ()-[r:OP]->() SET r.vt = date('2000-01-01')")
    with pytest.raises(kglite.CypherExecutionError, match="OP relationship from node '1' to node '10'"):
        g.cypher(
            "MATCH (s:Status {id: 1}), (c:Co {id: 10}) "
            "MERGE (s)-[:OP {vf: date('2003-01-01'), vt: date('2002-01-01')}]->(c)"
        )
    assert _ops(g) == 1
    g.cypher("MATCH ()-[r:OP]->() SET r.vt = null")
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_gaining_a_declared_label_answers_to_it(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    g.cypher("CREATE (:Draft {id: 7, vf: date('2011-01-01'), vt: date('2010-01-01')})")
    with pytest.raises(kglite.CypherExecutionError, match="node '7'"):
        g.cypher("MATCH (d:Draft {id: 7}) SET d:Status")
    assert g.cypher("MATCH (s:Status) RETURN count(s) AS n").to_list() == [{"n": 2}]
