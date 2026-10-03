"""``empty_when: 'to_before_from'``: under ``closed``, a date ``to`` exactly the
day before a date ``from`` is kept as an empty interval — valid on no day,
counted in ``empty_rows`` and warned about once — where ``closed`` otherwise
refuses the inverted row.

Surfaces: the ``db.temporal.declare`` key, the ``set_temporal`` / loader
kwarg, the blueprint ``temporal`` key, the ``db.temporal.declarations()``
column, and the saved file.

Red proof: before the option existed every route below refused the row as an
inverted interval (or rejected the key as unknown), and the procedure's
``declarations()`` row had no ``empty_when`` column.
"""

from __future__ import annotations

import json
import warnings

import pandas as pd
import pytest

import kglite
from kglite.blueprint import from_blueprint

SPANS = (
    "CREATE (:Span {id: 1, vf: date('2010-01-01'), vt: date('2010-12-31')}), "
    "(:Span {id: 2, vf: date('2011-03-05'), vt: date('2011-03-05')}), "
    "(:Span {id: 3, vf: date('2011-06-10'), vt: date('2011-06-09')})"
)
DECLARE = (
    "CALL db.temporal.declare({node: 'Span', from: 'vf', to: 'vt', convention: 'closed', empty_when: 'to_before_from'})"
)
EMPTY_WORDING = "under convention 'closed' with empty_when 'to_before_from'"


def _count(g, day):
    return g.cypher(f"FOR VALID_TIME AS OF date('{day}') MATCH (s:Span) RETURN count(s) AS n").to_list()[0]["n"]


def _declared():
    g = kglite.KnowledgeGraph()
    g.cypher(SPANS)
    result = g.cypher(DECLARE)
    return g, result


def test_procedure_key_accepts_the_row_warns_and_counts() -> None:
    g, result = _declared()
    assert result.to_list()[0]["declared"] is True
    warnings_ = [w for w in result.warnings if "empty interval" in w]
    assert len(warnings_) == 1, result.warnings
    assert warnings_[0].startswith(f"1 of 3 rows of node label 'Span' have an empty interval {EMPTY_WORDING}"), (
        warnings_
    )
    row = g.cypher("CALL db.temporal.declarations() YIELD name, convention, empty_when, empty_rows RETURN *").to_list()[
        0
    ]
    assert row == {"name": "Span", "convention": "closed", "empty_when": "to_before_from", "empty_rows": 1}


def test_the_empty_row_is_valid_on_no_day_and_outside_every_count() -> None:
    g, _ = _declared()
    for day in ("2011-06-08", "2011-06-09", "2011-06-10", "2011-06-11"):
        assert _count(g, day) == 0, day
    assert _count(g, "2010-06-01") == 1
    assert _count(g, "2011-03-05") == 1
    assert g.cypher(
        "MATCH (s:Span {id: 3}) RETURN valid_at(s, date('2011-06-09')) AS a, valid_at(s, date('2011-06-10')) AS b"
    ).to_list() == [{"a": False, "b": False}]
    # History still reads it.
    assert g.cypher("FOR VALID_TIME ALL MATCH (s:Span) RETURN count(s) AS n").to_list() == [{"n": 3}]


def test_a_cypher_write_keeps_the_row_with_one_warning() -> None:
    g, _ = _declared()
    result = g.cypher(
        "CREATE (:Span {id: 4, vf: date('2012-01-02'), vt: date('2012-01-01')}), "
        "(:Span {id: 5, vf: date('2012-02-01'), vt: date('2012-02-28')})"
    )
    empty = [w for w in result.warnings if "empty interval" in w]
    assert len(empty) == 1 and empty[0].startswith(f"1 of 2 rows written have an empty interval {EMPTY_WORDING}"), (
        result.warnings
    )
    assert g.cypher("CALL db.temporal.declarations() YIELD empty_rows RETURN empty_rows").to_list() == [
        {"empty_rows": 2}
    ]


def test_closed_without_the_option_still_refuses() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher(SPANS)
    with pytest.raises(Exception, match="is after the to bound"):
        g.cypher("CALL db.temporal.declare({node: 'Span', from: 'vf', to: 'vt', convention: 'closed'})")
    g2 = kglite.KnowledgeGraph()
    g2.cypher("CREATE (:Span {id: 1, vf: date('2010-01-01'), vt: date('2010-12-31')})")
    g2.cypher("CALL db.temporal.declare({node: 'Span', from: 'vf', to: 'vt', convention: 'closed'})")
    with pytest.raises(Exception, match="is after the to bound"):
        g2.cypher("CREATE (:Span {id: 3, vf: date('2011-06-10'), vt: date('2011-06-09')})")


@pytest.mark.parametrize(
    "vf,vt",
    [
        ("date('2011-06-10')", "date('2011-06-08')"),
        ("datetime('2011-06-10T00:00:00')", "date('2011-06-09')"),
        ("date('2011-06-10')", "datetime('2011-06-09T23:59:59')"),
        ("datetime('2011-06-10T08:00:00')", "datetime('2011-06-09T08:00:00')"),
    ],
)
def test_a_wider_inversion_or_a_timestamp_bound_is_still_refused(vf, vt) -> None:
    g, _ = _declared()
    with pytest.raises(Exception, match="is after the to bound"):
        g.cypher(f"CREATE (:Span {{id: 9, vf: {vf}, vt: {vt}}})")


def test_half_open_with_the_option_is_refused_with_a_clear_message() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher(SPANS)
    with pytest.raises(Exception, match=r"empty_when 'to_before_from' applies to convention 'closed'.*needs no option"):
        g.cypher(
            "CALL db.temporal.declare({node: 'Span', from: 'vf', to: 'vt', "
            "convention: 'half_open', empty_when: 'to_before_from'})"
        )
    assert g.cypher("CALL db.temporal.declarations() YIELD name RETURN name").to_list() == []


def test_an_unknown_spelling_is_refused() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher(SPANS)
    with pytest.raises(Exception, match="empty_when 'whenever' is not 'to_before_from'"):
        g.cypher(
            "CALL db.temporal.declare({node: 'Span', from: 'vf', to: 'vt', "
            "convention: 'closed', empty_when: 'whenever'})"
        )


# ── set_temporal and the loaders ─────────────────────────────────────────


def test_set_temporal_kwarg_declares_and_warns() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher(SPANS)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g.set_temporal("Span", "vf", "vt", convention="closed", empty_when="to_before_from")
    messages = [str(w.message) for w in caught]
    assert any(f"1 of 3 rows of node label 'Span' have an empty interval {EMPTY_WORDING}" in m for m in messages), (
        messages
    )
    assert _count(g, "2011-06-10") == 0
    # Re-declaring with no arguments keeps the declaration (a no-op); naming the
    # convention alone is a different one and conflicts.
    g.set_temporal("Span", "vf", "vt")
    with pytest.raises(Exception, match="already declared.*empty_when 'to_before_from'"):
        g.set_temporal("Span", "vf", "vt", convention="closed")


def test_set_temporal_refuses_half_open_with_the_option_and_a_bad_spelling() -> None:
    g = kglite.KnowledgeGraph()
    g.cypher(SPANS)
    with pytest.raises(Exception, match="needs no option"):
        g.set_temporal("Span", "vf", "vt", convention="half_open", empty_when="to_before_from")
    with pytest.raises(Exception, match="empty_when must be 'to_before_from'"):
        g.set_temporal("Span", "vf", "vt", empty_when="sometimes")


def _frame(rows):
    return pd.DataFrame(
        {
            "id": [r[0] for r in rows],
            "vf": pd.to_datetime([r[1] for r in rows]),
            "vt": pd.to_datetime([r[2] for r in rows]),
        }
    )


def test_add_nodes_kwarg_loads_the_row_and_warns_once() -> None:
    g = kglite.KnowledgeGraph()
    frame = _frame([(1, "2010-01-01", "2010-12-31"), (3, "2011-06-10", "2011-06-09")])
    types = {"vf": "validFrom", "vt": "validTo"}
    with pytest.raises(Exception, match="is after the to bound"):
        g.add_nodes(frame, "Span", "id", column_types=types, convention="closed")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g.add_nodes(frame, "Span", "id", column_types=types, convention="closed", empty_when="to_before_from")
    messages = [str(w.message) for w in caught if "empty interval" in str(w.message)]
    assert len(messages) == 1 and EMPTY_WORDING in messages[0], messages
    assert g.cypher("CALL db.temporal.declarations() YIELD empty_when, empty_rows RETURN *").to_list() == [
        {"empty_when": "to_before_from", "empty_rows": 1}
    ]
    assert _count(g, "2011-06-10") == 0


def test_the_loader_kwarg_needs_validity_column_types() -> None:
    g = kglite.KnowledgeGraph()
    with pytest.raises(ValueError, match="convention and empty_when apply to the validity interval"):
        g.add_nodes(_frame([(1, "2010-01-01", "2010-12-31")]), "Span", "id", empty_when="to_before_from")


# ── Blueprint ────────────────────────────────────────────────────────────


def _blueprint(tmp_path, temporal):
    pd.DataFrame(
        {
            "sid": [1, 2],
            "sf": ["2010-01-01", "2011-06-10"],
            "st": ["2010-12-31", "2011-06-09"],
        }
    ).to_csv(tmp_path / "span.csv", index=False)
    bp = {
        "settings": {"root": str(tmp_path)},
        "nodes": {
            "Span": {
                "csv": "span.csv",
                "pk": "sid",
                "title": "sid",
                "properties": {"sf": "validFrom", "st": "validTo"},
                "temporal": temporal,
            }
        },
    }
    path = tmp_path / "blueprint.json"
    path.write_text(json.dumps(bp), encoding="utf-8")
    return path


def test_blueprint_key_declares_and_keeps_the_row(tmp_path) -> None:
    path = _blueprint(tmp_path, {"from": "sf", "to": "st", "convention": "closed", "empty_when": "to_before_from"})
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        g = from_blueprint(path, save=False)
    row = g.cypher("CALL db.temporal.declarations() YIELD empty_when, empty_rows RETURN *").to_list()
    assert row == [{"empty_when": "to_before_from", "empty_rows": 1}]
    assert any(EMPTY_WORDING in str(w.message) for w in caught), [str(w.message) for w in caught]
    assert g.cypher("FOR VALID_TIME ALL MATCH (s:Span) RETURN count(s) AS n").to_list() == [{"n": 2}]
    assert 'temporal_empty_when="to_before_from"' in g.describe()


def test_blueprint_without_the_key_refuses_the_inverted_row(tmp_path) -> None:
    path = _blueprint(tmp_path, {"from": "sf", "to": "st", "convention": "closed"})
    with pytest.raises(Exception, match="is after the to bound"):
        from_blueprint(path, save=False)


def test_blueprint_refuses_half_open_with_the_option_and_a_bad_spelling(tmp_path) -> None:
    path = _blueprint(tmp_path, {"from": "sf", "to": "st", "convention": "half_open", "empty_when": "to_before_from"})
    with pytest.raises(Exception, match="applies to convention 'closed'.*needs no option"):
        from_blueprint(path, save=False)
    path = _blueprint(tmp_path, {"from": "sf", "to": "st", "convention": "closed", "empty_when": "later"})
    with pytest.raises(Exception, match="empty_when 'later' is not 'to_before_from'"):
        from_blueprint(path, save=False)


# ── Persistence ──────────────────────────────────────────────────────────


def test_the_option_survives_a_save_and_load(tmp_path) -> None:
    g, _ = _declared()
    target = str(tmp_path / "spans.kgl")
    g.save(target)
    loaded = kglite.load(target)
    row = loaded.cypher("CALL db.temporal.declarations() YIELD empty_when, empty_rows RETURN *").to_list()
    assert row == [{"empty_when": "to_before_from", "empty_rows": 1}]
    assert _count(loaded, "2011-06-10") == 0
    # The loaded declaration still gates writes the same way.
    with pytest.raises(Exception, match="is after the to bound"):
        loaded.cypher("CREATE (:Span {id: 7, vf: date('2011-06-10'), vt: date('2011-06-01')})")
