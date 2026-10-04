"""Peer-label checks on guarded per-edge counters: a peer qualifies through a
primary type, a secondary label, an alternation branch or an extra label.

The label constraint is resolved once per operator (keys interned, secondary
carrier buckets looked up), so a compare that looked only at the primary type
would drop every peer that carries the pattern's label as a secondary one.

Fixture (``Employee`` declared with ``vf``/``vt``, closed):

* employee 1 expired in 2010 and carries the undeclared secondary label ``Tag``;
  employee 2 is open.
* contractor 5 expired in 2010 and carries ``Employee`` as a secondary label;
  contractor 6 is open and carries nothing.
* all four sit in dept 10 through ``IN``; employee 2 and contractor 6 also
  belong to project 20 through ``ON``.

Each golden is ``(today, FOR VALID_TIME ALL)``; the same statement with every
optimiser pass disabled must agree with both.

Red proof: a primary-type-only peer compare turns ``Employee`` under ALL from
3 into 2 and the ``Tag`` / ``Contractor:Employee`` rows into 0.
"""

from __future__ import annotations

import contextlib
import os
import tempfile

import pytest

import kglite

MODES = ("memory", "mapped", "disk")
ALL = "FOR VALID_TIME ALL "

SETUP = [
    "CREATE (:Employee {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), "
    "(:Employee {id: 2, vf: date('2005-01-01')}), "
    "(:Contractor {id: 5, vf: date('2000-01-01'), vt: date('2010-01-01')}), "
    "(:Contractor {id: 6}), (:Dept {id: 10}), (:Project {id: 20})",
    "MATCH (w), (f:Dept) WHERE w:Employee OR w:Contractor CREATE (w)-[:IN]->(f)",
    "MATCH (w), (p:Project) WHERE w.id IN [2, 6] CREATE (w)-[:ON]->(p)",
    "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', convention: 'closed'})",
    "MATCH (r:Contractor {id: 5}) SET r:Employee",
    "MATCH (w:Employee {id: 1}) SET w:Tag",
]

# peer pattern -> (today, ALL)
PEERS = {
    "p:Employee": (1, 3),
    "p:Contractor": (1, 2),
    "p:Employee|Contractor": (2, 4),
    "p:Contractor:Employee": (0, 1),
    "p:Tag": (0, 1),
    "p:Dept": (0, 0),
    "p": (2, 4),
}


@contextlib.contextmanager
def _graph(mode: str):
    with tempfile.TemporaryDirectory() as directory:
        if mode == "mapped":
            g = kglite.KnowledgeGraph(storage="mapped")
        elif mode == "disk":
            g = kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "graph"))
        else:
            g = kglite.KnowledgeGraph()
        for query in SETUP:
            g.cypher(query).to_list()
        yield g


def _scalar(g, query, prefix="", **kwargs):
    rows = g.cypher(prefix + query, **kwargs).to_list()
    assert len(rows) == 1, rows
    return rows[0]["n"]


SHAPES = {
    "optional": "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN]-({peer}) RETURN count(p) AS n",
    "optional_distinct": "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN]-({peer}) RETURN count(DISTINCT p) AS n",
    "with": "MATCH (d:Dept) OPTIONAL MATCH (d)<-[:IN]-({peer}) WITH d, count(p) AS c RETURN sum(c) AS n",
    "match": "MATCH (d:Dept)<-[:IN]-({peer}) RETURN count(p) AS n",
    "chain": "MATCH (d:Dept)<-[:IN]-({peer})-[:ON]->(x:Project) RETURN count(p) AS n",
}
# the chain shape only reaches employee 2 and contractor 6 through `ON`
CHAIN_ADJUST = {"p:Employee": (1, 1), "p:Contractor": (1, 1), "p:Employee|Contractor": (2, 2)}


def _expected(shape, peer):
    if shape == "chain":
        if peer in CHAIN_ADJUST:
            return CHAIN_ADJUST[peer]
        return {"p:Contractor:Employee": (0, 0), "p:Tag": (0, 0), "p:Dept": (0, 0), "p": (2, 2)}[peer]
    return PEERS[peer]


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("shape", sorted(SHAPES))
@pytest.mark.parametrize("peer", sorted(PEERS))
def test_peer_label_counts(mode, shape, peer):
    query = SHAPES[shape].format(peer=peer)
    today, everything = _expected(shape, peer)
    off = kglite.cypher_pass_names()
    with _graph(mode) as g:
        assert _scalar(g, query) == today
        assert _scalar(g, query, ALL) == everything
        assert _scalar(g, query, disabled_passes=off) == today
        assert _scalar(g, query, ALL, disabled_passes=off) == everything
