"""Relationship counts under a valid-time context equal a hand computation.

``MATCH ()-[r:T]->() RETURN count(r)`` (and ``count(*)``, the untyped form,
the undirected form, the per-type grouping) is answered from the masks of the
context instead of testing every relationship, so each golden here compares
the engine with an independent Python oracle over the fixture's rows, with
every optimiser pass disabled, and against ``FOR VALID_TIME ALL``.

Fixture: departments ``d1`` (to 2010-12-31), ``d2`` (from 2011-01-01), ``d3``
(from 2020-01-01); employees ``e1`` (from 2005-01-01), ``e2`` (from
2013-01-01), ``e3`` (2012-01-01 to 2012-06-30); an undeclared ``Contractor``.

* ``ASSIGNED`` is declared for ``Employee`` sources only: the contractor's
  ``ASSIGNED`` rows carry bounds that would hide them if the declaration were
  applied to every source, and only their endpoint can hide them.
* ``REPORTS`` is declared for every source type.
* ``KNOWS`` is undeclared; endpoints alone decide it.

Red proof: dropping the endpoint test from the stored count turns ``KNOWS`` at
2012-06-15 from 1 into 4; applying ``ASSIGNED``'s bounds to contractor sources
turns it from 3 into 1.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import os
import tempfile

import pytest

import kglite

MODES = ("memory", "mapped", "disk")
CONVENTIONS = ("closed", "half_open")
ALL = "FOR VALID_TIME ALL "

# label -> (id, from, to); `Contractor` is undeclared so its bounds never count.
NODES = [
    ("Department", "d1", "2000-01-01", "2010-12-31"),
    ("Department", "d2", "2011-01-01", None),
    ("Department", "d3", "2020-01-01", None),
    ("Employee", "e1", "2005-01-01", None),
    ("Employee", "e2", "2013-01-01", None),
    ("Employee", "e3", "2012-01-01", "2012-06-30"),
    ("Contractor", "c1", "2030-01-01", "2031-01-01"),
]
LABEL = {nid: label for label, nid, _, _ in NODES}
NODE_BOUNDS = {nid: (f, t) for label, nid, f, t in NODES if label != "Contractor"}

# type, source, target, from, to
EDGES = [
    ("ASSIGNED", "e1", "d2", "2011-01-01", None),
    ("ASSIGNED", "e1", "d1", "2005-01-01", None),
    ("ASSIGNED", "e3", "d2", "2012-01-01", "2012-12-31"),
    ("ASSIGNED", "e1", "d2", "2015-01-01", None),
    ("ASSIGNED", "e2", "d2", "2012-01-01", None),
    ("ASSIGNED", "e2", "d3", "2012-01-01", None),
    ("ASSIGNED", "c1", "d1", "2030-01-01", None),
    ("ASSIGNED", "c1", "d2", "2000-01-01", "2001-01-01"),
    ("REPORTS", "e1", "e3", "2005-01-01", None),
    ("REPORTS", "e2", "e1", "2013-01-01", "2014-01-01"),
    ("REPORTS", "c1", "e1", "2020-01-01", None),
    ("REPORTS", "e1", "e1", "2006-01-01", None),
    ("KNOWS", "e1", "e3", None, None),
    ("KNOWS", "e2", "e2", None, None),
    ("KNOWS", "d1", "e1", None, None),
    ("KNOWS", "c1", "c1", None, None),
]
DECLARED_EDGE_SOURCE = {"ASSIGNED": "Employee", "REPORTS": None}
TYPES = ("ASSIGNED", "REPORTS", "KNOWS")

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


def _date(text):
    return dt.date.fromisoformat(text) if text else None


def _within(bounds, t, convention):
    start, end = (_date(b) for b in bounds)
    if start is not None and t < start:
        return False
    if end is None:
        return True
    return t <= end if convention == "closed" else t < end


def _node_valid(nid, t, convention):
    return nid not in NODE_BOUNDS or _within(NODE_BOUNDS[nid], t, convention)


def _admitted(edge, t, convention):
    ty, source, target, start, end = edge
    if ty in DECLARED_EDGE_SOURCE:
        scope = DECLARED_EDGE_SOURCE[ty]
        if scope in (None, LABEL[source]) and not _within((start, end), t, convention):
            return False
    return _node_valid(source, t, convention) and _node_valid(target, t, convention)


def _oracle(t, convention):
    """Per type: (admitted, admitted self-loops)."""
    out = {ty: [0, 0] for ty in TYPES}
    for edge in EDGES:
        if _admitted(edge, t, convention):
            out[edge[0]][0] += 1
            out[edge[0]][1] += edge[1] == edge[2]
    return out


def _statements(convention):
    def props(f, t):
        parts = []
        if f:
            parts.append(f"from: date('{f}')")
        if t:
            parts.append(f"to: date('{t}')")
        return ", ".join(parts)

    out = [
        "CREATE "
        + ", ".join(
            f"(:{label} {{id: '{nid}', f: date('{f}')" + (f", t: date('{t}')" if t else "") + "})"
            for label, nid, f, t in NODES
        )
    ]
    for ty, source, target, f, t in EDGES:
        out.append(
            f"MATCH (a {{id: '{source}'}}), (b {{id: '{target}'}}) CREATE (a)-[:{ty} {{{props(f, t)}}}]->(b)"
            if (f or t)
            else f"MATCH (a {{id: '{source}'}}), (b {{id: '{target}'}}) CREATE (a)-[:{ty}]->(b)"
        )
    for label in ("Department", "Employee"):
        out.append(f"CALL db.temporal.declare({{node: '{label}', from: 'f', to: 't', convention: '{convention}'}})")
    out.append(
        "CALL db.temporal.declare({relationship: 'ASSIGNED', source_type: 'Employee', "
        f"from: 'from', to: 'to', convention: '{convention}'}})"
    )
    out.append(
        f"CALL db.temporal.declare({{relationship: 'REPORTS', from: 'from', to: 'to', convention: '{convention}'}})"
    )
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


def _n(g, query, **kwargs):
    rows = g.cypher(query, **kwargs).to_list()
    assert len(rows) == 1, rows
    return rows[0]["n"]


def _expected_forms(t, convention):
    counts = _oracle(t, convention)
    forms = {}
    for ty in TYPES:
        n, loops = counts[ty]
        forms[f"MATCH ()-[r:{ty}]->() RETURN count(r) AS n"] = n
        forms[f"MATCH ()-[r:{ty}]->() RETURN count(*) AS n"] = n
        forms[f"MATCH ()-[r:{ty}]-() RETURN count(r) AS n"] = 2 * n - loops
    forms["MATCH ()-[r]->() RETURN count(r) AS n"] = sum(n for n, _ in counts.values())
    forms["MATCH ()-[r]->() RETURN count(*) AS n"] = sum(n for n, _ in counts.values())
    return forms


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("convention", CONVENTIONS)
def test_counts_equal_the_hand_computation_at_every_instant(mode, convention):
    off = kglite.cypher_pass_names()
    with _graph(mode, convention) as g:
        for instant in INSTANTS:
            forms = _expected_forms(_date(instant), convention)
            for query, expected in forms.items():
                assert _n(g, query, valid_at=instant) == expected, (instant, query)
                # a repeat statement reads what the first one stored
                assert _n(g, query, valid_at=instant) == expected, (instant, query, "repeat")
                assert _n(g, query, valid_at=instant, disabled_passes=off) == expected, (instant, query, "off")


@pytest.mark.parametrize("mode", MODES)
def test_the_default_context_is_today(mode):
    today = dt.datetime.now(dt.timezone.utc).date()
    with _graph(mode, "closed") as g:
        for query, expected in _expected_forms(today, "closed").items():
            assert _n(g, query) == expected, query


@pytest.mark.parametrize("mode", MODES)
def test_for_valid_time_all_counts_every_relationship(mode):
    with _graph(mode, "closed") as g:
        for ty in TYPES:
            total = sum(1 for e in EDGES if e[0] == ty)
            assert _n(g, ALL + f"MATCH ()-[r:{ty}]->() RETURN count(r) AS n") == total
        assert _n(g, ALL + "MATCH ()-[r]->() RETURN count(r) AS n") == len(EDGES)


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("convention", CONVENTIONS)
def test_the_per_type_grouping_equals_the_hand_computation(mode, convention):
    with _graph(mode, convention) as g:
        for instant in INSTANTS:
            counts = _oracle(_date(instant), convention)
            rows = g.cypher("MATCH ()-[r]->() RETURN type(r) AS t, count(r) AS n", valid_at=instant).to_list()
            assert {row["t"]: row["n"] for row in rows} == {ty: c[0] for ty, c in counts.items() if c[0]}, instant


@pytest.mark.parametrize("mode", MODES)
def test_a_write_moves_the_stored_answer(mode):
    query = "MATCH ()-[r:KNOWS]->() RETURN count(r) AS n"
    with _graph(mode, "closed") as g:
        before = _n(g, query, valid_at="2012-06-15")
        assert before == _oracle(_date("2012-06-15"), "closed")["KNOWS"][0]
        g.cypher("MATCH (a {id: 'e1'}), (b {id: 'e3'}) CREATE (a)-[:KNOWS]->(b)").to_list()
        assert _n(g, query, valid_at="2012-06-15") == before + 1
