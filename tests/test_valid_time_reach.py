"""Which declared targets a statement's scopes reach under the valid-time
context, and what each statement therefore sees.

A node declaration on label L governs every node carrying L, primary or
secondary. A label no node carries as a secondary one reaches itself and the
declared labels some node carries as secondary ones; a label some node does
carry as a secondary one can sit on a node of any type, so it reaches every
declared label. A scope that reaches nothing declared plans and runs as it
would without the context, whatever other scope the statement has.

Absolute expected values per storage mode, under the default context (today)
and under ``FOR VALID_TIME ALL``. The fixture:

* ``Employee`` is declared (``vf``/``vt``, closed); employee 1 expired in 2010, employee 2
  is open.
* ``Contractor`` is not declared; contractor 5 expired in 2010 by the same two properties and
  carries ``Employee`` as a secondary label, contractor 6 is open and carries nothing.
* employee 1 carries the undeclared secondary label ``Tag``.
* ``Dept`` 10 is only ever a primary type; both employees sit in it through ``IN``.

Red proof: against a template that stays wide whenever secondary labels exist
the echo goldens fail (a ``Dept`` statement lists ``(:Employee)`` as a target);
against one that never widens, the contractor 5 / ``Tag`` rows leak.
"""

from __future__ import annotations

import contextlib
import os
import tempfile

import pytest

import kglite

pytestmark = pytest.mark.parity

MODES = ("memory", "mapped", "disk")
ALL = "FOR VALID_TIME ALL "

SETUP = [
    "CREATE (:Employee {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), "
    "(:Employee {id: 2, vf: date('2005-01-01')}), "
    "(:Contractor {id: 5, vf: date('2000-01-01'), vt: date('2010-01-01')}), "
    "(:Contractor {id: 6}), (:Dept {id: 10, vf: date('2001-01-01'), vt: date('2003-01-01')})",
    "MATCH (w:Employee), (f:Dept) CREATE (w)-[:IN]->(f)",
    "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', convention: 'closed'})",
]
LABELS = [
    "MATCH (r:Contractor {id: 5}) SET r:Employee",
    "MATCH (w:Employee {id: 1}) SET w:Tag",
]


@contextlib.contextmanager
def _graph(mode: str, labels=LABELS):
    with tempfile.TemporaryDirectory() as directory:
        if mode == "mapped":
            g = kglite.KnowledgeGraph(storage="mapped")
        elif mode == "disk":
            g = kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "graph"))
        else:
            g = kglite.KnowledgeGraph()
        for query in SETUP + list(labels):
            g.cypher(query).to_list()
        yield g


def _col(result, key="id"):
    return sorted(row[key] for row in result.to_list())


def _both(g, query, key="id"):
    return _col(g.cypher(query), key), _col(g.cypher(ALL + query), key)


@pytest.mark.parametrize("mode", MODES)
def test_secondary_label_reach_goldens(mode):
    with _graph(mode) as g:
        # Undeclared primary carrying a declared secondary: hidden through it.
        assert _both(g, "MATCH (r:Contractor) RETURN r.id AS id") == ([6], [5, 6])
        # Declared primary carrying an undeclared secondary, queried by it.
        assert _both(g, "MATCH (t:Tag) RETURN t.id AS id") == ([], [1])
        # Queried by the declared label: primaries and secondary carriers.
        assert _both(g, "MATCH (n:Employee) RETURN n.id AS id") == ([2], [1, 2, 5])
        # A type nothing declared can reach.
        assert _both(g, "MATCH (f:Dept) RETURN f.id AS id") == ([10], [10])
        # Alternation and conjunction reach through every label named.
        assert _both(g, "MATCH (n:Contractor|Dept) RETURN n.id AS id") == ([6, 10], [5, 6, 10])
        assert _both(g, "MATCH (n:Contractor:Employee) RETURN n.id AS id") == ([], [5])
        assert _both(g, "MATCH (n:Tag|Dept) RETURN n.id AS id") == ([10], [1, 10])


def _echo(g, query):
    return g.cypher(query).diagnostics["temporal"]


@pytest.mark.parametrize("mode", MODES)
def test_the_echo_names_only_the_targets_a_statement_reaches(mode):
    # An undeclared secondary label (`Tag`) alone does not widen a statement
    # that names a type nothing declared can sit on.
    with _graph(mode, labels=LABELS[1:]) as g:
        field = _echo(g, "MATCH (f:Dept) RETURN f.id")
        assert field["targets"] == [] and field["hidden"] == {} and field["route"] == "plain"
        assert field["source"] == "default"
        assert _echo(g, "MATCH (n:Employee) RETURN n.id")["targets"] == ["(:Employee)"]
        assert _echo(g, "MATCH (t:Tag) RETURN t.id")["targets"] == ["(:Employee)"]
        nothing = _echo(g, "RETURN 1")
        assert nothing["targets"] == [] and nothing["route"] == "plain"
        assert nothing["instant"] == field["instant"]
    # A declared label carried as a secondary one is reached through every type.
    with _graph(mode) as g:
        field = _echo(g, "MATCH (f:Dept) RETURN f.id")
        assert field["targets"] == ["(:Employee)"] and field["route"] == "guarded"
        contractor = _echo(g, "MATCH (r:Contractor) RETURN r.id")
        assert contractor["targets"] == ["(:Employee)"]
        # Disk builds no endpoint index, so its targets carry no count.
        assert contractor["hidden"] == ({} if mode == "disk" else {"(:Employee)": 2})


@pytest.mark.parametrize("mode", MODES)
def test_a_guarded_arm_under_an_unguarded_top_level(mode):
    with _graph(mode) as g:
        union = "MATCH (f:Dept) RETURN f.id AS id UNION MATCH (w:Employee) RETURN w.id AS id"
        assert _both(g, union) == ([2, 10], [1, 2, 5, 10])
        call = "MATCH (f:Dept) CALL { MATCH (w:Employee) RETURN count(w) AS c } RETURN f.id AS id, c"
        assert g.cypher(call).to_list() == [{"id": 10, "c": 1}]
        assert g.cypher(ALL + call).to_list() == [{"id": 10, "c": 3}]
        # The unguarded arm keeps every pass while the guarded one stays filtered.
        union_counts = "MATCH (f:Dept) RETURN count(f) AS n UNION ALL MATCH (w:Employee) RETURN count(w) AS n"
        assert sorted(_col(g.cypher(union_counts), "n")) == [1, 1]
        assert sorted(_col(g.cypher(ALL + union_counts), "n")) == [1, 3]


@pytest.mark.parametrize("mode", MODES)
def test_an_unguarded_body_takes_the_fusion_a_guarded_scope_is_denied(mode):
    # No declared label is carried as a secondary one, so the body reaches
    # nothing declared and plans with every pass while the top level, which
    # names `Employee`, runs filtered.
    with _graph(mode, labels=LABELS[1:]) as g:
        query = (
            "MATCH (w:Employee) CALL { UNWIND [date('2002-06-01'), date('2020-01-01')] AS d "
            "MATCH (f:Dept) WHERE valid_at(f, d, 'vf', 'vt') RETURN f.id AS fid } "
            "RETURN w.id AS wid, fid"
        )
        at = "FOR VALID_TIME AS OF date('2015-01-01') "
        rows = g.cypher(at + query).to_list()
        assert rows == [{"wid": 2, "fid": 10}]
        plan = [row["operation"] for row in g.cypher("EXPLAIN " + at + query).to_list()]
        assert plan[0].startswith("ValidTimeContext") and "targets=(:Employee" in plan[0]


@pytest.mark.parametrize("mode", MODES)
def test_an_optional_match_joining_an_undeclared_to_a_declared_type(mode):
    with _graph(mode) as g:
        query = "MATCH (f:Dept) OPTIONAL MATCH (f)<-[:IN]-(w:Employee) RETURN f.id AS id, count(w) AS c"
        assert g.cypher(query).to_list() == [{"id": 10, "c": 1}]
        assert g.cypher(ALL + query).to_list() == [{"id": 10, "c": 2}]
        shaped = "MATCH (f:Dept) OPTIONAL MATCH (f)<-[:IN]-(w:Employee) RETURN f.id AS id, collect(w.id) AS ws"
        assert g.cypher(shaped).to_list() == [{"id": 10, "ws": [2]}]


@pytest.mark.parametrize("mode", MODES)
def test_patterns_inside_expressions_reach_their_labels(mode):
    with _graph(mode) as g:
        count = "MATCH (f:Dept) RETURN COUNT { (f)<-[:IN]-(:Employee) } AS c"
        assert g.cypher(count).to_list() == [{"c": 1}]
        assert g.cypher(ALL + count).to_list() == [{"c": 2}]
        exists = "MATCH (f:Dept) RETURN EXISTS { MATCH (f)<-[:IN]-(w:Employee) WHERE w.id = 1 } AS e"
        assert g.cypher(exists).to_list() == [{"e": False}]
        assert g.cypher(ALL + exists).to_list() == [{"e": True}]
        comprehension = "MATCH (f:Dept) RETURN [(f)<-[:IN]-(w:Employee) | w.id] AS ws"
        assert g.cypher(comprehension).to_list() == [{"ws": [2]}]
        assert sorted(g.cypher(ALL + comprehension).to_list()[0]["ws"]) == [1, 2]


RIG = "MATCH (r:Contractor) RETURN r.id AS id"


@pytest.mark.parametrize("mode", MODES)
def test_a_cached_plan_follows_label_writes(mode):
    with _graph(mode, labels=()) as g:
        # No secondary label yet: the statement reaches nothing declared, and
        # the plan cached for it holds no guard.
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [5, 6]
        g.cypher("MATCH (r:Contractor {id: 5}) SET r:Employee").to_list()
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [6]
        g.cypher("MATCH (r:Contractor {id: 5}) REMOVE r:Employee").to_list()
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [5, 6]
        # The same through the loader-style batch API.
        g.add_label("Contractor", [5], "Employee")
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [6]
        g.remove_label("Contractor", [5], "Employee")
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [5, 6]


@pytest.mark.parametrize("mode", MODES)
def test_a_transaction_reads_its_own_label_writes_before_commit(mode):
    with _graph(mode, labels=()) as g:
        assert _col(g.cypher(RIG)) == [5, 6]
        tx = g.begin()
        assert _col(tx.cypher(RIG)) == [5, 6]
        tx.cypher("MATCH (r:Contractor {id: 5}) SET r:Employee").to_list()
        assert _col(tx.cypher(RIG)) == [6]
        tx.cypher("MATCH (r:Contractor {id: 5}) REMOVE r:Employee").to_list()
        assert _col(tx.cypher(RIG)) == [5, 6]
        tx.rollback()
        # Nothing the rolled-back transaction did reaches the graph.
        assert _col(g.cypher(RIG)) == [5, 6]
        with g.begin() as tx:
            tx.cypher("MATCH (r:Contractor {id: 5}) SET r:Employee").to_list()
            assert _col(tx.cypher(RIG)) == [6]
        assert _col(g.cypher(RIG)) == [6]


@pytest.mark.parametrize("mode", MODES)
def test_a_declaration_made_after_a_plan_was_cached_applies(mode):
    with _graph(mode, labels=()) as g:
        g.cypher("MATCH (r:Contractor {id: 5}) SET r:Employee").to_list()
        g.cypher("CALL db.temporal.undeclare({node: 'Employee'})").to_list()
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [5, 6]
        g.cypher("CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', convention: 'closed'})").to_list()
        for _ in range(2):
            assert _col(g.cypher(RIG)) == [6]


@pytest.mark.parametrize("mode", MODES)
def test_a_capped_start_scan_still_finds_a_late_match(mode):
    """The guarded start set is cut at the row cap's headroom (1000 sources for a
    LIMIT 1). Every source is admitted yet none but the last has a hop, so the
    capped pass comes back empty and only the uncapped retry finds the row: the
    scan must report that it dropped admitted sources."""
    with _graph(mode, labels=()) as g:
        g.cypher("UNWIND range(100, 1599) AS i CREATE (:Employee {id: i, vf: date('2000-01-01')})").to_list()
        g.cypher("UNWIND range(100, 2099) AS i CREATE (:Site {id: i})").to_list()
        g.cypher("MATCH (w:Employee {id: 1599}), (p:Site {id: 100}) CREATE (w)-[:ON]->(p)").to_list()
        query = "MATCH (w:Employee)-[:ON]->(p:Site) RETURN w.id AS w LIMIT 1"
        assert g.cypher(query).to_list() == [{"w": 1599}]
        assert g.cypher(ALL + query).to_list() == [{"w": 1599}]
