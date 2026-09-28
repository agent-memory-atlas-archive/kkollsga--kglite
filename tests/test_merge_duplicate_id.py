"""A node ``MERGE`` never creates a second node under an id its type already
holds. ``MERGE (n:A:B {id: x})`` matches only a node carrying both labels, and
one naming other properties only a node holding them; when the ``A`` node
with id ``x`` lacks them, creating would fork the id, so the statement is
refused naming what differs and how to write it instead. ``CREATE`` keeps its
opt-in rule (a duplicate id warns).

Red proof: before the refusal each refused ``MERGE`` below created a second
node with the id, reported only by the duplicate-id warning.
"""

from __future__ import annotations

import pytest

import kglite

MODES = pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])


def _graph(mode, tmp_path) -> kglite.KnowledgeGraph:
    if mode == "memory":
        g = kglite.KnowledgeGraph()
    elif mode == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    g.cypher("CREATE (:A {id: 'x', name: 'first'}), (:A {id: 7, name: 'seven'})").to_list()
    return g


def _count(g) -> int:
    """Every ``A`` node — an id lookup would collapse a forked id to one."""
    return g.cypher("MATCH (n:A) RETURN count(n) AS c").to_list()[0]["c"]


@MODES
def test_a_missing_label_refuses_instead_of_forking_the_id(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path)
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"MERGE would create a second :A node with id 'x'; the existing node lacks label :B "
        r"— match on the id and add the label \(`MERGE \(n:A \{id: 'x'\}\) SET n:B`\), or use ON MATCH SET",
    ):
        g.cypher("MERGE (n:A:B {id: 'x'}) RETURN n.id")
    assert _count(g) == 2
    with pytest.raises(kglite.CypherExecutionError, match="lacks label :B, :C"):
        g.cypher("MERGE (n:A:B:C {id: 7}) RETURN n.id")
    assert _count(g) == 2


@MODES
def test_matching_on_the_id_and_adding_the_label_then_matches(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path)
    g.cypher("MERGE (n:A {id: 'x'}) SET n:B").to_list()
    rows = g.cypher("MERGE (n:A:B {id: 'x'}) RETURN n.name AS name, labels(n) AS labels").to_list()
    assert rows == [{"name": "first", "labels": ["A", "B"]}]
    g.cypher("MERGE (n:A {id: 7}) ON MATCH SET n:B").to_list()
    assert g.cypher("MATCH (n:B) RETURN count(n) AS c").to_list() == [{"c": 2}]
    assert _count(g) == 2


@MODES
def test_a_differing_property_refuses_naming_it(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path)
    with pytest.raises(
        kglite.CypherExecutionError,
        match=r"MERGE would create a second :A node with id 'x'; the existing node differs in name "
        r"— match on the id alone and set the properties \(`MERGE \(n:A \{id: 'x'\}\) SET n.name = …`\)",
    ):
        g.cypher("MERGE (n:A {id: 'x', name: 'second'})")
    assert g.cypher("MERGE (n:A {id: 'x', name: 'first'}) RETURN n.name AS n").to_list() == [{"n": "first"}]
    assert _count(g) == 2


@MODES
def test_a_fresh_id_still_creates(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path)
    g.cypher("MERGE (n:A:B {id: 'y', name: 'new'})").to_list()
    g.cypher("UNWIND [8, 9] AS k MERGE (n:A {id: k})").to_list()
    assert g.cypher("MATCH (n:A) RETURN count(n) AS c").to_list() == [{"c": 5}]
    # A MERGE without an id keys on its properties, as before.
    g.cypher("MERGE (n:A {name: 'first'})").to_list()
    assert g.cypher("MATCH (n:A) RETURN count(n) AS c").to_list() == [{"c": 5}]


@MODES
def test_a_refusal_rolls_the_statement_back(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match="lacks label :B"):
        g.cypher("UNWIND ['z', 'x'] AS k MERGE (n:A:B {id: k})")
    assert g.cypher("MATCH (n:A) RETURN count(n) AS c").to_list() == [{"c": 2}]


@MODES
def test_create_still_forks(mode, tmp_path) -> None:
    """``CREATE`` keeps the opt-in rule; the duplicate-id warning it earns is
    pinned in ``test_create_duplicate_id_warning.py``."""
    g = _graph(mode, tmp_path)
    g.cypher("CREATE (:A:B {id: 'x'})").to_list()
    assert _count(g) == 3


@MODES
def test_a_declared_type_keys_a_version_on_its_own_id(mode, tmp_path) -> None:
    """The version rule keys relationships; a node's id is its identity, so a
    MERGE naming another version's bounds under a held id is refused — a new
    version takes its own id."""
    g = _graph(mode, tmp_path)
    g.cypher("CREATE (:Status {id: 1, entity: 'e', vf: date('2000-01-01'), vt: date('2010-01-01')})").to_list()
    g.cypher("CALL db.temporal.declare({node: 'Status', from: 'vf', to: 'vt', convention: 'half_open'})").to_list()
    with pytest.raises(
        kglite.CypherExecutionError, match=r"second :Status node with id 1; the existing node differs in vf"
    ):
        g.cypher("MERGE (s:Status {id: 1, entity: 'e', vf: date('2010-01-01')})")
    g.cypher("MERGE (s:Status {id: 2, entity: 'e', vf: date('2010-01-01')})").to_list()
    assert g.cypher("MATCH (s:Status) RETURN count(s) AS c").to_list() == [{"c": 2}]


@MODES
def test_a_loaded_integer_id_matches_beside_other_properties(mode, tmp_path) -> None:
    """A loaded integer id is stored in the loader's compact kind; the id
    index reads `1` as that id, and so does a pattern naming the id beside
    other properties. It compared the kinds exactly, missed the node and
    (before the refusal) created a second one under the id."""
    import pandas as pd

    g = _graph(mode, tmp_path)
    g.add_nodes(pd.DataFrame({"id": [101], "name": ["loaded"]}), "L", "id")
    for literal in ("101", "101.0"):
        rows = g.cypher(f"MERGE (n:L {{id: {literal}, name: 'loaded'}}) RETURN n.name AS name").to_list()
        assert rows == [{"name": "loaded"}], literal
    assert g.cypher("MATCH (n:L) RETURN count(n) AS c").to_list() == [{"c": 1}]
