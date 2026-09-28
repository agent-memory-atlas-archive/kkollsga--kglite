"""A writer that gives a node a declared label answers to that label's
declaration, as Cypher ``SET n:Label`` does: ``add_nodes(labels=[…])`` judges
its rows (and an existing node that gains the label) before writing,
``add_label`` judges each node that gains the label, and a blueprint's
``labels``, ``from_records`` labels and ontology materialisation (a
materialised ancestor stamped on new and existing nodes) do the same.

An inverted interval is refused; an empty one under ``half_open`` (from =
to) gains the label, with one warning naming the first such node, and is
counted in ``empty_rows``.

Red proof: ``add_nodes(labels=…)`` and ``add_label`` stamped the label with no
check, leaving a row under ``Status`` that ``db.temporal.declarations()``
then counted in ``empty_rows``; ontology materialisation and the ancestor
stamped on a new node did likewise. Before the empty rule changed, every
empty row below was refused.
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

COUNTS = "CALL db.temporal.declarations() YIELD name, empty_rows RETURN name, empty_rows"


def _graph(storage, tmp_path) -> kglite.KnowledgeGraph:
    if storage == "disk":
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    elif storage == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph()
    g.cypher(
        "CREATE (:Status {id: 1, vf: date('2000-01-01'), vt: date('2005-01-01')}), "
        "(:Other {id: 50, vf: date('2010-01-01'), vt: date('2000-01-01')}), "
        "(:Other {id: 51, vf: date('2010-01-01'), vt: null})"
    ).to_list()
    g.cypher("CALL db.temporal.declare({node: 'Status', from: 'vf', to: 'vt', convention: 'half_open'})").to_list()
    return g


def _status(g) -> list:
    return sorted(r["id"] for r in g.cypher("MATCH (s:Status) RETURN s.id AS id").to_list())


def _clean(g) -> None:
    assert all(r["empty_rows"] == 0 for r in g.cypher(COUNTS).to_list())


def _empty_rows(g) -> dict:
    return {r["name"]: r["empty_rows"] for r in g.cypher(COUNTS).to_list()}


def _caught(call) -> list[str]:
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        call()
    return [str(w.message) for w in caught if "empty interval" in str(w.message)]


def _frame(rows):
    return pd.DataFrame(
        {
            "id": [r[0] for r in rows],
            "vf": pd.to_datetime([r[1] for r in rows]),
            "vt": pd.to_datetime([r[2] for r in rows]),
        }
    )


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_add_nodes_with_a_declared_label_judges_its_rows(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    inverted = _frame([(60, "2001-01-01", None), (61, "2010-01-01", "2001-01-01")])
    with pytest.raises(kglite.ArgumentError, match=r"row 1 \(0-based\) of the load, .*is after the to bound"):
        g.add_nodes(inverted, "Other", "id", labels=["Status"])
    assert g.cypher("MATCH (o:Other) RETURN count(o) AS c").to_list() == [{"c": 2}]
    # An existing node that gains the label answers with its stored bounds.
    with pytest.raises(kglite.ArgumentError, match=r"row 0 \(0-based\) of the load, .*is after the to bound"):
        g.add_nodes(pd.DataFrame({"id": [50], "name": ["x"]}), "Other", "id", labels=["Status"])
    g.add_nodes(_frame([(62, "2001-01-01", "2002-01-01")]), "Other", "id", labels=["Status"])
    g.add_nodes(pd.DataFrame({"id": [51]}), "Other", "id", labels=["Status"])
    assert _status(g) == [1, 51, 62]
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_add_label_judges_each_node_that_gains_it(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    with pytest.raises(kglite.ArgumentError, match=r"node '50', the from bound .*is after the to bound"):
        g.add_label("Other", [51, 50], "Status")
    assert _status(g) == [1]
    assert g.add_label("Other", [51], "Status")["labelled"] == 1
    assert _status(g) == [1, 51]
    # The same rule as the Cypher twin.
    with pytest.raises(kglite.CypherExecutionError, match="node '50'"):
        g.cypher("MATCH (o:Other {id: 50}) SET o:Status")
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_ontology_materialisation_judges_the_ancestor_it_stamps(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    g.define_ontology({"classes": {"Status": {}, "Other": {"is_a": "Status"}}})
    with pytest.raises(Exception, match=r"materializing label 'Status': node '50'"):
        g.materialize_ontology()
    assert _status(g) == [1]
    g.cypher("MATCH (o:Other {id: 50}) SET o.vt = null").to_list()
    g.materialize_ontology()
    assert _status(g) == [1, 50, 51]
    # A new node of the subclass is born with the ancestor: judged by it.
    with pytest.raises(kglite.CypherExecutionError, match="node '70'"):
        g.cypher("CREATE (:Other {id: 70, vf: date('2010-01-01'), vt: date('2000-01-01')})")
    with pytest.raises(kglite.ArgumentError, match=r"row 0 \(0-based\) of the load"):
        g.add_nodes(_frame([(71, "2010-01-01", "2000-01-01")]), "Other", "id")
    assert _status(g) == [1, 50, 51]
    _clean(g)


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_an_empty_row_gains_a_declared_label_with_one_warning(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    rows = _frame([(63, "2003-01-01", "2003-01-01"), (64, "2004-01-01", None), (65, "2005-01-01", "2005-01-01")])
    caught = _caught(lambda: g.add_nodes(rows, "Other", "id", labels=["Status"]))
    assert len(caught) == 1, caught
    assert caught[0].startswith("2 of 3 rows written have an empty interval under convention 'half_open'"), caught
    assert "the first is row 0 (0-based) of the load" in caught[0], caught
    assert _status(g) == [1, 63, 64, 65]
    g.cypher("CREATE (:Other {id: 66, vf: date('2006-01-01'), vt: date('2006-01-01')})").to_list()
    caught = _caught(lambda: g.add_label("Other", [66], "Status"))
    assert len(caught) == 1 and "the first is node '66'" in caught[0], caught
    assert _status(g) == [1, 63, 64, 65, 66]
    assert _empty_rows(g) == {"Status": 3}
    assert g.cypher("MATCH (s:Status) RETURN s.id AS id", valid_at="2006-01-01").to_list() == [{"id": 64}]


@pytest.mark.parametrize("storage", MODES, ids=MODE_IDS)
def test_ontology_materialisation_stamps_an_empty_row(storage, tmp_path) -> None:
    g = _graph(storage, tmp_path)
    g.cypher("MATCH (o:Other {id: 50}) SET o.vt = o.vf").to_list()
    g.define_ontology({"classes": {"Status": {}, "Other": {"is_a": "Status"}}})
    g.materialize_ontology()
    assert _status(g) == [1, 50, 51]
    assert _empty_rows(g) == {"Status": 1}


def test_a_blueprint_label_onto_a_declared_type_is_refused(tmp_path) -> None:
    pd.DataFrame({"sid": [1], "vf": ["2000-01-01"], "vt": ["2001-01-01"]}).to_csv(tmp_path / "status.csv", index=False)
    pd.DataFrame({"oid": [9], "vf": ["2010-01-01"], "vt": ["2001-01-01"]}).to_csv(tmp_path / "other.csv", index=False)
    bp = {
        "settings": {"root": str(tmp_path)},
        "nodes": {
            "Status": {
                "csv": "status.csv",
                "pk": "sid",
                "properties": {"vf": "date", "vt": "date"},
                "temporal": {"from": "vf", "to": "vt", "convention": "closed"},
            },
            "Other": {
                "csv": "other.csv",
                "pk": "oid",
                "properties": {"vf": "date", "vt": "date"},
                "labels": ["Status"],
            },
        },
    }
    path = tmp_path / "blueprint.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        with pytest.raises(Exception, match="node '9'.*is after the to bound"):
            from_blueprint(path, save=False)


def test_extend_judges_a_label_it_unions_onto_a_declared_label() -> None:
    target = _graph(None, None)
    source = kglite.KnowledgeGraph()
    source.cypher("CREATE (:Other:Status {id: 80, vf: date('2010-01-01'), vt: date('2000-01-01')})").to_list()
    with pytest.raises(Exception, match=r"extend: node '80', the from bound .*is after the to bound"):
        target.extend(source)
    assert target.cypher("MATCH (o:Other) RETURN count(o) AS c").to_list() == [{"c": 2}]
    assert _status(target) == [1]
    source = kglite.KnowledgeGraph()
    source.cypher("CREATE (:Other:Status {id: 81, vf: date('2010-01-01'), vt: date('2010-01-01')})").to_list()
    caught = _caught(lambda: target.extend(source))
    assert len(caught) == 1 and "node '81'" in caught[0], caught
    assert _status(target) == [1, 81]
    assert _empty_rows(target) == {"Status": 1}


def test_a_blueprint_label_keeps_an_empty_row_with_a_warning(tmp_path) -> None:
    pd.DataFrame({"sid": [1], "vf": ["2000-01-01"], "vt": ["2001-01-01"]}).to_csv(tmp_path / "status.csv", index=False)
    pd.DataFrame({"oid": [9], "vf": ["2010-01-01"], "vt": ["2010-01-01"]}).to_csv(tmp_path / "other.csv", index=False)
    bp = {
        "settings": {"root": str(tmp_path)},
        "nodes": {
            "Status": {
                "csv": "status.csv",
                "pk": "sid",
                "properties": {"vf": "date", "vt": "date"},
                "temporal": {"from": "vf", "to": "vt", "convention": "half_open"},
            },
            "Other": {
                "csv": "other.csv",
                "pk": "oid",
                "properties": {"vf": "date", "vt": "date"},
                "labels": ["Status"],
            },
        },
    }
    path = tmp_path / "blueprint.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = from_blueprint(path, save=False)
    empty = [str(w.message) for w in caught if "empty interval" in str(w.message)]
    assert len(empty) == 1 and "node '9'" in empty[0], [str(w.message) for w in caught]
    assert _status(g) == [1, 9]
    assert _empty_rows(g) == {"Status": 1}
