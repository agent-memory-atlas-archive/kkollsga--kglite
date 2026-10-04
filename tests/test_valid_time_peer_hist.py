"""Grouped counts answered from a cached per-node histogram equal a hand count.

``MATCH (e:Employee)-[:T]->(d:Department) RETURN d.id, count(e)`` (and its
``WITH`` and ``OPTIONAL MATCH`` forms) counts, per group node, the admitted
relationships to peers of one label. On an in-memory graph a cached histogram
answers it once the walks it replaces have cost as much as building it; these
goldens force the build (``KGLITE_PEER_HIST_BUILD_AFTER_NS=0``) and compare it
with an independent Python oracle over the fixture's rows, with the walk (the
build never reached), with every optimiser pass disabled, and with
``FOR VALID_TIME ALL``.

Fixture: departments ``d1`` (to 2010-12-31), ``d2`` (from 2011-01-01), ``d3``
(from 2020-01-01); employees ``e1`` (from 2005-01-01), ``e2`` (from
2013-01-01), ``e3`` (2012-01-01 to 2012-06-30); contractors ``c1`` (undeclared,
never hidden) and ``c2``, which carries ``Employee`` as a second label so its
own bounds apply and a peer test for ``Employee`` must find it there.

* ``ASSIGNED`` is declared for ``Employee`` sources only; ``REPORTS`` for every
  source; ``KNOWS`` is undeclared, so only endpoints decide it.
* Repeated relationships between one pair, a self-loop and a relationship
  whose endpoint is hidden are all present.

Red proof (mutations of the histogram, each run against this file): serving
after a write without the version check, leaving the endpoint masks out of the
build, and testing no peer label each turn goldens red.
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
BUILD_AFTER = "KGLITE_PEER_HIST_BUILD_AFTER_NS"

NODES = [
    ("Department", "d1", "2000-01-01", "2010-12-31"),
    ("Department", "d2", "2011-01-01", None),
    ("Department", "d3", "2020-01-01", None),
    ("Employee", "e1", "2005-01-01", None),
    ("Employee", "e2", "2013-01-01", None),
    ("Employee", "e3", "2012-01-01", "2012-06-30"),
    ("Contractor", "c1", "2030-01-01", "2031-01-01"),
    ("Contractor", "c2", "2012-01-01", "2013-06-30"),
]
PRIMARY = {nid: label for label, nid, _, _ in NODES}
LABELS = {nid: {label} for label, nid, _, _ in NODES}
LABELS["c2"].add("Employee")
# Nodes whose bounds count: a declared primary label, or a declared second one.
BOUNDS = {nid: (f, t) for label, nid, f, t in NODES if label != "Contractor" or nid == "c2"}

# type, source, target, from, to
EDGES = [
    ("ASSIGNED", "e1", "d2", "2011-01-01", None),
    ("ASSIGNED", "e1", "d1", "2005-01-01", None),
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
    ("REPORTS", "c2", "e1", "2012-06-01", None),
    ("KNOWS", "e1", "e3", None, None),
    ("KNOWS", "e2", "e2", None, None),
    ("KNOWS", "d1", "e1", None, None),
    ("KNOWS", "c1", "c1", None, None),
    ("KNOWS", "e1", "c2", None, None),
    ("KNOWS", "c2", "e2", None, None),
    ("KNOWS", "c2", "c2", None, None),
    ("KNOWS", "d1", "c1", None, None),
    ("KNOWS", "d2", "c1", None, None),
    ("KNOWS", "c1", "d2", None, None),
    ("KNOWS", "c2", "c1", None, None),
    ("KNOWS", "c1", "c2", None, None),
    ("KNOWS", "d3", "d3", None, None),
    ("KNOWS", "d1", "d2", None, None),
]
DECLARED_EDGE_SOURCE = {"ASSIGNED": "Employee", "REPORTS": None}

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

# name -> (query, group label, relationship type, direction from the group,
#          peer labels (any-of) or None, optional match)
SHAPES = {
    "return_in": (
        "MATCH (e:Employee)-[:ASSIGNED]->(d:Department) RETURN d.id AS g, count(e) AS n",
        "Department",
        "ASSIGNED",
        "in",
        {"Employee"},
        False,
    ),
    "return_star": (
        "MATCH (e:Employee)-[:ASSIGNED]->(d:Department) RETURN d.id AS g, count(*) AS n",
        "Department",
        "ASSIGNED",
        "in",
        {"Employee"},
        False,
    ),
    "with_in": (
        "MATCH (e:Employee)-[:ASSIGNED]->(d:Department) WITH d, count(e) AS n RETURN d.id AS g, n",
        "Department",
        "ASSIGNED",
        "in",
        {"Employee"},
        False,
    ),
    "optional_in": (
        "MATCH (d:Department) OPTIONAL MATCH (d)<-[:ASSIGNED]-(e:Employee) RETURN d.id AS g, count(e) AS n",
        "Department",
        "ASSIGNED",
        "in",
        {"Employee"},
        True,
    ),
    "alternation_peer": (
        "MATCH (d:Department)<-[:ASSIGNED]-(x:Employee|Contractor) RETURN d.id AS g, count(x) AS n",
        "Department",
        "ASSIGNED",
        "in",
        {"Employee", "Contractor"},
        False,
    ),
    "contractor_peer": (
        "MATCH (d:Department)<-[:ASSIGNED]-(c:Contractor) RETURN d.id AS g, count(c) AS n",
        "Department",
        "ASSIGNED",
        "in",
        {"Contractor"},
        False,
    ),
    "with_contractor_peer": (
        "MATCH (d:Department)<-[:ASSIGNED]-(c:Contractor) WITH d, count(c) AS n RETURN d.id AS g, n",
        "Department",
        "ASSIGNED",
        "in",
        {"Contractor"},
        False,
    ),
    "out_with_secondary_peer": (
        "MATCH (a:Employee)-[:REPORTS]->(b:Employee) RETURN a.id AS g, count(b) AS n",
        "Employee",
        "REPORTS",
        "out",
        {"Employee"},
        False,
    ),
    "undirected_self_loop": (
        "MATCH (a:Employee)-[:REPORTS]-(b:Employee) RETURN a.id AS g, count(b) AS n",
        "Employee",
        "REPORTS",
        "both",
        {"Employee"},
        False,
    ),
    "undeclared_type": (
        "MATCH (a:Employee)-[:KNOWS]->(b:Employee) RETURN b.id AS g, count(a) AS n",
        "Employee",
        "KNOWS",
        "in",
        {"Employee"},
        False,
    ),
    "undeclared_type_undirected": (
        "MATCH (a:Employee)-[:KNOWS]-(b:Employee) WITH a, count(b) AS n RETURN a.id AS g, n",
        "Employee",
        "KNOWS",
        "both",
        {"Employee"},
        False,
    ),
    "contractor_out": (
        "MATCH (c:Contractor)-[:KNOWS]->(x:Contractor) RETURN c.id AS g, count(x) AS n",
        "Contractor",
        "KNOWS",
        "out",
        {"Contractor"},
        False,
    ),
    "department_undirected": (
        "MATCH (d:Department)-[:KNOWS]-(x:Department) RETURN d.id AS g, count(x) AS n",
        "Department",
        "KNOWS",
        "both",
        {"Department"},
        False,
    ),
    "mixed_undirected_with": (
        "MATCH (d:Department)-[:KNOWS]-(x:Contractor) WITH d, count(x) AS n RETURN d.id AS g, n",
        "Department",
        "KNOWS",
        "both",
        {"Contractor"},
        False,
    ),
    "optional_secondary_peer": (
        "MATCH (a:Employee) OPTIONAL MATCH (a)<-[:REPORTS]-(b:Employee) RETURN a.id AS g, count(b) AS n",
        "Employee",
        "REPORTS",
        "in",
        {"Employee"},
        True,
    ),
    "optional_out_contractor": (
        "MATCH (a:Employee) OPTIONAL MATCH (a)-[:KNOWS]->(b:Contractor) RETURN a.id AS g, count(b) AS n",
        "Employee",
        "KNOWS",
        "out",
        {"Contractor"},
        True,
    ),
    "optional_undirected_contractor": (
        "MATCH (a:Employee) OPTIONAL MATCH (a)-[:KNOWS]-(b:Contractor) RETURN a.id AS g, count(b) AS n",
        "Employee",
        "KNOWS",
        "both",
        {"Contractor"},
        True,
    ),
    "any_peer": (
        "MATCH (a)-[:KNOWS]->(b:Employee) RETURN b.id AS g, count(a) AS n",
        "Employee",
        "KNOWS",
        "in",
        None,
        False,
    ),
}


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
    return t is None or nid not in BOUNDS or _within(BOUNDS[nid], t, convention)


def _admitted(edge, t, convention):
    ty, source, target, start, end = edge
    if t is None:
        return True
    if ty in DECLARED_EDGE_SOURCE:
        scope = DECLARED_EDGE_SOURCE[ty]
        if scope in (None, PRIMARY[source]) and not _within((start, end), t, convention):
            return False
    return _node_valid(source, t, convention) and _node_valid(target, t, convention)


def _count(node, ty, direction, peers, t, convention):
    total = 0
    for edge in EDGES:
        if edge[0] != ty or not _admitted(edge, t, convention):
            continue
        _, source, target, _, _ = edge
        legs = []
        if direction in ("out", "both") and source == node:
            legs.append(target)
        if direction in ("in", "both") and target == node and not (direction == "both" and source == target):
            legs.append(source)
        total += sum(1 for peer in legs if peers is None or LABELS[peer] & peers)
    return total


def _oracle(shape, t, convention):
    _, group, ty, direction, peers, optional = SHAPES[shape]
    out = {}
    for nid in PRIMARY:
        if group not in LABELS[nid] or not _node_valid(nid, t, convention):
            continue
        n = _count(nid, ty, direction, peers, t, convention)
        if n or optional:
            out[nid] = n
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
        ),
        "MATCH (c {id: 'c2'}) SET c:Employee",
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


@pytest.fixture(autouse=True)
def _build_at_once(monkeypatch):
    monkeypatch.setenv(BUILD_AFTER, "0")


def _run(g, shape, instant, **kwargs):
    query = SHAPES[shape][0]
    if instant == "all":
        rows = g.cypher(ALL + query, **kwargs).to_list()
    else:
        rows = g.cypher(query, valid_at=instant, **kwargs).to_list()
    return {row["g"]: row["n"] for row in rows}


def _expected(shape, instant, convention):
    return _oracle(shape, None if instant == "all" else _date(instant), convention)


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("convention", CONVENTIONS)
@pytest.mark.parametrize("shape", SHAPES)
def test_grouped_counts_equal_the_hand_count_at_every_instant(mode, convention, shape):
    off = kglite.cypher_pass_names()
    with _graph(mode, convention) as g:
        for instant in [*INSTANTS, "all"]:
            expected = _expected(shape, instant, convention)
            assert _run(g, shape, instant) == expected, (instant, "built")
            assert _run(g, shape, instant) == expected, (instant, "repeat")
            assert _run(g, shape, instant, disabled_passes=off) == expected, (instant, "passes off")


@pytest.mark.parametrize("shape", SHAPES)
def test_the_walk_agrees_with_the_histogram(shape, monkeypatch):
    with _graph("memory", "closed") as g:
        built = {i: _run(g, shape, i) for i in [*INSTANTS, "all"]}
        monkeypatch.setenv(BUILD_AFTER, str(2**62))
        with _graph("memory", "closed") as walked:
            assert {i: _run(walked, shape, i) for i in [*INSTANTS, "all"]} == built


@pytest.mark.parametrize("shape", SHAPES)
def test_the_default_context_is_today(shape):
    today = dt.datetime.now(dt.timezone.utc).date()
    with _graph("memory", "closed") as g:
        query = SHAPES[shape][0]
        rows = g.cypher(query).to_list()
        assert {row["g"]: row["n"] for row in rows} == _oracle(shape, today, "closed")


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("instant", ["2012-06-15", "all"])
@pytest.mark.parametrize(
    ("shape", "gained"),
    [("return_in", 2), ("with_in", 2), ("optional_in", 2), ("contractor_peer", 1), ("with_contractor_peer", 1)],
)
def test_a_write_between_two_counts_moves_the_answer(mode, instant, shape, gained):
    with _graph(mode, "closed") as g:
        before = _run(g, shape, instant)
        assert before == _expected(shape, instant, "closed")
        assert _run(g, shape, instant) == before
        # e1, c2 and d2 are valid at both instants, so each new relationship
        # counts: for an `Employee` peer both (c2 carries the label second),
        # for a `Contractor` peer only c2's.
        g.cypher(
            "MATCH (a {id: 'e1'}), (b {id: 'd2'}) CREATE (a)-[:ASSIGNED {from: date('1999-12-31')}]->(b)"
        ).to_list()
        g.cypher(
            "MATCH (a {id: 'c2'}), (b {id: 'd2'}) CREATE (a)-[:ASSIGNED {from: date('1999-12-31')}]->(b)"
        ).to_list()
        after = _run(g, shape, instant)
        assert after["d2"] == before.get("d2", 0) + gained
        assert _run(g, shape, instant) == after
        g.cypher("MATCH (a)-[r:ASSIGNED]->(b {id: 'd2'}) WHERE r.from = date('1999-12-31') DELETE r").to_list()
        assert _run(g, shape, instant) == before


@pytest.mark.parametrize("instant", ["2012-06-15", "all"])
def test_a_node_label_write_moves_the_answer(instant):
    """A peer's label is part of what is cached: gaining it changes the count."""
    shape = "with_contractor_peer"
    off = kglite.cypher_pass_names()
    with _graph("memory", "closed") as g:
        before = _run(g, shape, instant)
        assert _run(g, shape, instant) == before
        g.cypher("MATCH (n {id: 'e3'}) SET n:Contractor").to_list()
        after = _run(g, shape, instant)
        assert after == _run(g, shape, instant, disabled_passes=off)
        assert after.get("d2", 0) == before.get("d2", 0) + 1


@pytest.mark.parametrize("shape", ["return_in", "optional_in", "undirected_self_loop"])
def test_a_frozen_view_and_its_source_keep_their_own_answers(shape):
    with _graph("memory", "closed") as g:
        frozen = g.freeze()
        query = SHAPES[shape][0]
        at = "2012-06-15"
        first = {r["g"]: r["n"] for r in frozen.cypher(query, valid_at=at).to_list()}
        assert first == _expected(shape, at, "closed")
        g.cypher("MATCH (a {id: 'e1'}), (b {id: 'e3'}) CREATE (a)-[:REPORTS {from: date('2005-01-01')}]->(b)").to_list()
        g.cypher(
            "MATCH (a {id: 'e1'}), (b {id: 'd2'}) CREATE (a)-[:ASSIGNED {from: date('2005-01-01')}]->(b)"
        ).to_list()
        for _ in range(2):
            assert {r["g"]: r["n"] for r in frozen.cypher(query, valid_at=at).to_list()} == first
        moved = {r["g"]: r["n"] for r in g.cypher(query, valid_at=at).to_list()}
        assert moved != first
        copy = g.copy()
        assert {r["g"]: r["n"] for r in copy.cypher(query, valid_at=at).to_list()} == moved
        copy.cypher(
            "MATCH (a {id: 'e1'})-[r:ASSIGNED]->(b {id: 'd2'}) WHERE r.from = date('2005-01-01') DELETE r"
        ).to_list()
        assert {r["g"]: r["n"] for r in g.cypher(query, valid_at=at).to_list()} == moved


def test_two_instants_on_one_plan_do_not_share_an_answer():
    """The default context resolves per execution: a plan reused across a day
    boundary must not read the earlier day's histogram."""
    with _graph("memory", "closed") as g:
        shape = "return_in"
        answers = []
        for instant in ["2005-06-01", "2012-06-15", "2005-06-01", "2012-06-15"]:
            answers.append(_run(g, shape, instant))
        assert answers[0] == answers[2] == _expected(shape, "2005-06-01", "closed")
        assert answers[1] == answers[3] == _expected(shape, "2012-06-15", "closed")
        assert answers[0] != answers[1]
