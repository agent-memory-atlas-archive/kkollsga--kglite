"""Node counts under a valid-time context equal the nodes a plain match returns.

``MATCH (n) RETURN count(n)`` is one popcount over the context's node mask
and ``(n:T)`` / ``(n:A|B)`` / the per-type grouping read a per-label count the
masks keep, so each golden compares the count with ``len(MATCH ... RETURN n)``
under the same context, on every storage mode, both interval conventions and
nine instants.

Fixture: declared ``Employee`` and ``Department`` rows with bounds; an
undeclared ``Contractor`` primary, one of which carries the declared
``Employee`` label as a secondary label (so its bounds hide it); a ``Tag``
secondary label on an employee; an undeclared ``Project`` type; and nodes
created then deleted, leaving vacant slots among the live ones.

Red proof: not adding the vacant-slot correction (counting ``node_bound``
minus the cleared bits) turns the untyped count wrong after the deletes;
ignoring the secondary buckets turns ``(n:Employee)`` short by the carrying
contractor; both are mutation-checked in the commit that added this file.
"""

from __future__ import annotations

import contextlib
import os
import tempfile

import pytest

import kglite

MODES = ("memory", "mapped", "disk")
CONVENTIONS = ("closed", "half_open")
ALL = "FOR VALID_TIME ALL "
INSTANTS = [
    "2001-01-01",
    "2005-06-01",
    "2010-12-31",
    "2011-01-01",
    "2012-06-15",
    "2012-06-30",
    "2013-01-01",
    "2020-06-01",
    "2025-01-01",
]

FORMS = {
    "all": ("MATCH (n) RETURN count(n) AS n", "MATCH (n) RETURN n.id AS id"),
    "all_star": ("MATCH (n) RETURN count(*) AS n", "MATCH (n) RETURN n.id AS id"),
    "employee": ("MATCH (n:Employee) RETURN count(n) AS n", "MATCH (n:Employee) RETURN n.id AS id"),
    "department": ("MATCH (n:Department) RETURN count(n) AS n", "MATCH (n:Department) RETURN n.id AS id"),
    "contractor": ("MATCH (n:Contractor) RETURN count(n) AS n", "MATCH (n:Contractor) RETURN n.id AS id"),
    "project": ("MATCH (n:Project) RETURN count(n) AS n", "MATCH (n:Project) RETURN n.id AS id"),
    "tag": ("MATCH (n:Tag) RETURN count(n) AS n", "MATCH (n:Tag) RETURN n.id AS id"),
    "alternation": (
        "MATCH (n:Department|Project) RETURN count(n) AS n",
        "MATCH (n:Department|Project) RETURN n.id AS id",
    ),
    "absent": ("MATCH (n:Nothing) RETURN count(n) AS n", "MATCH (n:Nothing) RETURN n.id AS id"),
}


def _statements(convention):
    def node(label, nid, f=None, t=None):
        props = [f"id: '{nid}'"]
        if f:
            props.append(f"f: date('{f}')")
        if t:
            props.append(f"t: date('{t}')")
        return f"(:{label} {{{', '.join(props)}}})"

    created = [
        node("Department", "d1", "2000-01-01", "2010-12-31"),
        node("Department", "d2", "2011-01-01"),
        node("Department", "d3", "2020-01-01"),
        node("Employee", "e1", "2005-01-01"),
        node("Employee", "e2", "2013-01-01"),
        node("Employee", "e3", "2012-01-01", "2012-06-30"),
        node("Contractor", "c1", "2012-01-01", "2012-06-30"),
        node("Contractor", "c2", "2030-01-01", "2031-01-01"),
        node("Project", "p1", "1990-01-01", "1991-01-01"),
        node("Project", "p2"),
        node("Employee", "x1", "2000-01-01"),
        node("Department", "x2", "2000-01-01"),
        node("Project", "x3"),
    ]
    out = ["CREATE " + ", ".join(created)]
    for label in ("Department", "Employee"):
        out.append(f"CALL db.temporal.declare({{node: '{label}', from: 'f', to: 't', convention: '{convention}'}})")
    out += [
        "MATCH (n {id: 'c1'}) SET n:Employee",
        "MATCH (n {id: 'e1'}) SET n:Tag",
        "MATCH (n {id: 'p2'}) SET n:Tag",
        # vacant slots inside the node range, then more live nodes after them
        "MATCH (n) WHERE n.id IN ['x1', 'x2', 'x3'] DETACH DELETE n",
        "CREATE " + ", ".join([node("Employee", "e4", "2012-06-01", "2012-12-31"), node("Project", "p3")]),
    ]
    return out


@contextlib.contextmanager
def _graph(mode, convention):
    with tempfile.TemporaryDirectory() as directory:
        if mode == "mapped":
            g = kglite.KnowledgeGraph(storage="mapped")
        elif mode == "disk":
            g = kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "graph"))
        else:
            g = kglite.KnowledgeGraph()
        for statement in _statements(convention):
            g.cypher(statement).to_list()
        yield g


def _count(g, query, **kwargs):
    rows = g.cypher(query, **kwargs).to_list()
    assert len(rows) == 1, rows
    return rows[0]["n"]


def _check(g, **context):
    off = kglite.cypher_pass_names()
    for name, (count_query, rows_query) in FORMS.items():
        expected = len(g.cypher(rows_query, disabled_passes=off, **context).to_list())
        assert _count(g, count_query, **context) == expected, (name, context)
        assert _count(g, count_query, **context) == expected, (name, context, "repeat")
        assert _count(g, count_query, disabled_passes=off, **context) == expected, (name, context, "off")
    grouped = g.cypher("MATCH (n) RETURN n.type AS t, count(*) AS n", **context).to_list()
    per_type = {row["t"]: row["n"] for row in grouped}
    plain = g.cypher("MATCH (n) RETURN n.type AS t", disabled_passes=off, **context).to_list()
    expected_types: dict = {}
    for row in plain:
        expected_types[row["t"]] = expected_types.get(row["t"], 0) + 1
    assert per_type == expected_types, context


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("convention", CONVENTIONS)
def test_counts_equal_the_matcher_at_every_instant(mode, convention):
    with _graph(mode, convention) as g:
        for instant in INSTANTS:
            _check(g, valid_at=instant)


@pytest.mark.parametrize("mode", MODES)
def test_all_and_the_default_context(mode):
    with _graph(mode, "closed") as g:
        _check(g, valid_at="ALL")
        _check(g)
        # thirteen rows created, three deleted, two more created: 12 live
        assert _count(g, ALL + "MATCH (n) RETURN count(n) AS n") == 12


@pytest.mark.parametrize("mode", MODES)
def test_a_write_moves_the_stored_answer(mode):
    with _graph(mode, "closed") as g:
        before = _count(g, "MATCH (n) RETURN count(n) AS n", valid_at="2012-06-15")
        typed = _count(g, "MATCH (n:Employee) RETURN count(n) AS n", valid_at="2012-06-15")
        g.cypher("CREATE (:Employee {id: 'e9', f: date('2012-01-01')})").to_list()
        assert _count(g, "MATCH (n) RETURN count(n) AS n", valid_at="2012-06-15") == before + 1
        assert _count(g, "MATCH (n:Employee) RETURN count(n) AS n", valid_at="2012-06-15") == typed + 1
        g.cypher("MATCH (n {id: 'e9'}) DELETE n").to_list()
        assert _count(g, "MATCH (n) RETURN count(n) AS n", valid_at="2012-06-15") == before
        assert _check(g, valid_at="2012-06-15") is None


@pytest.mark.parametrize("convention", CONVENTIONS)
def test_a_filter_the_masks_cannot_decide_still_counts_the_nodes(monkeypatch, convention):
    # An index byte cap nothing fits leaves every declared label to the
    # validity evaluator: no decisive masks, so the counts walk the nodes.
    monkeypatch.setenv("KGLITE_TEMPORAL_INDEX_MAX_BYTES", "1")
    with _graph("memory", convention) as g:
        for instant in INSTANTS:
            _check(g, valid_at=instant)
