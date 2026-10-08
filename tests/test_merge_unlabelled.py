"""A label-less node ``MERGE`` is matched like a label-less ``MATCH``: against
every node, whatever its type.

Red proof: before the fix ``MERGE (b)`` consulted only the nodes created
without a label, so on a graph holding only ``:A`` and ``:B`` nodes it matched
nothing, created a fresh node, and every later row matched that one node.
"""

from __future__ import annotations

import pytest

import kglite

MODES = pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])


def _graph(mode, tmp_path, setup) -> kglite.KnowledgeGraph:
    if mode == "memory":
        g = kglite.KnowledgeGraph()
    elif mode == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    g.cypher(setup).to_list()
    return g


def _nodes(g) -> int:
    return g.cypher("MATCH (n) RETURN count(n) AS c").to_list()[0]["c"]


@MODES
def test_unlabelled_merge_matches_every_typed_node(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path, "CREATE (:A), (:B)")
    rows = g.cypher("MATCH (a) MERGE (b) RETURN count(*) AS c").to_list()
    assert rows == [{"c": 4}]
    assert _nodes(g) == 2


@MODES
def test_unlabelled_merge_rows_feed_a_following_optional_match(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path, "CREATE (a:A), (b:B), (a)-[:T1]->(b), (b)-[:T2]->(a)")
    rows = g.cypher("MATCH (a) MERGE (b) WITH * OPTIONAL MATCH (a)--(b) RETURN count(*) AS c").to_list()
    assert rows == [{"c": 6}]
    assert _nodes(g) == 2


@MODES
def test_unlabelled_merge_with_a_property_matches_a_typed_node(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path, "CREATE (:A {x: 1}), (:B {x: 2})")
    rows = g.cypher("MERGE (n {x: 2}) RETURN labels(n)[0] AS t").to_list()
    assert rows == [{"t": "B"}]
    assert _nodes(g) == 2


@MODES
def test_unlabelled_merge_creates_when_nothing_matches(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path, "CREATE (:A {x: 1})")
    g.cypher("MERGE (n {x: 9})").to_list()
    g.cypher("MERGE (n {x: 9})").to_list()
    assert _nodes(g) == 2
    assert g.cypher("MATCH (n {x: 9}) RETURN count(n) AS c").to_list() == [{"c": 1}]
