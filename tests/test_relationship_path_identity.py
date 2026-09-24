"""A path keeps the relationships its MATCH bound, not whatever later occupies
the same storage slots.

A path hop names a slot. Inside one write statement a ``DELETE`` frees a slot
and a following ``CREATE`` can be handed it back, so materialising the hop at
*use* time — which is what the executor did before — silently substituted the
replacement relationship into a path that never matched it, properties and all,
and handed it to the write procedures as if the MATCH had selected it.
"""

from __future__ import annotations

import pytest

import kglite
from kglite import KnowledgeGraph


def _graph() -> KnowledgeGraph:
    graph = KnowledgeGraph()
    graph.cypher("CREATE (a:N {id: 1}), (b:N {id: 2}), (a)-[:R {tag: 'old', text: 'alpha'}]->(b)")
    return graph


def test_path_hop_does_not_follow_a_reused_relationship_slot() -> None:
    graph = _graph()
    rows = graph.cypher(
        "MATCH p = (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag: 'fresh'}]->(b) "
        "WITH p, relationships(p) AS rels, relationships(p)[0] AS pr "
        "RETURN size(rels) AS n, pr.tag AS tag, pr.type AS rel_type"
    ).to_list()
    assert rows == [{"n": 1, "tag": None, "rel_type": "R"}]


def test_stale_path_hop_is_refused_by_edge_embedding_set() -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError, match="deleted or replaced earlier in this statement"):
        graph.cypher(
            "MATCH p = (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag: 'fresh'}]->(b) "
            "WITH p CALL db.relationship_embeddings.set({type: 'R', text_property: 'text', "
            "entries: [{relationship: relationships(p)[0], vector: [1.0, 0.0]}]}) "
            "YIELD stored RETURN stored"
        )
    assert graph.cypher("MATCH ()-[r:R]->() RETURN count(r) AS n").to_list() == [{"n": 1}]


def test_stale_path_hop_is_refused_by_delete() -> None:
    graph = _graph()
    with pytest.raises(kglite.CypherExecutionError, match="stale"):
        graph.cypher(
            "MATCH p = (a:N)-[r:R]->(b:N) DELETE r CREATE (a)-[fresh:R {tag: 'fresh'}]->(b) "
            "WITH p, relationships(p)[0] AS pr DELETE pr RETURN 1 AS done"
        )
    # What must never happen is the replacement being deleted in the stale
    # hop's place; the refused statement rolls back, so the match survives.
    assert graph.cypher("MATCH ()-[r:R]->() WHERE r.tag = 'old' RETURN count(r) AS n").to_list() == [{"n": 1}]


def test_path_bound_after_the_reuse_sees_the_fresh_relationship() -> None:
    graph = _graph()
    rows = graph.cypher(
        "MATCH (a:N)-[r:R]->(b:N) DELETE r "
        "CREATE (a)-[fresh:R {tag: 'fresh', text: 'beta'}]->(b) "
        "WITH a MATCH p = (a)-[:R]->() WITH p, relationships(p)[0] AS pr "
        "CALL db.relationship_embeddings.set({type: 'R', text_property: 'text', "
        "entries: [{relationship: pr, vector: [1.0, 0.0]}]}) YIELD stored "
        "RETURN pr.tag AS tag, stored"
    ).to_list()
    assert rows == [{"tag": "fresh", "stored": 1}]


def test_read_statement_path_relationships_are_unchanged() -> None:
    graph = _graph()
    rows = graph.cypher(
        "MATCH p = (a:N)-[r:R]->(b:N) WITH r, p, relationships(p)[0] AS pr "
        "RETURN r = pr AS eq, size(relationships(p)) AS n, pr.tag AS tag"
    ).to_list()
    assert rows == [{"eq": True, "n": 1, "tag": "old"}]


def test_variable_length_path_hops_carry_per_hop_tokens() -> None:
    graph = KnowledgeGraph()
    graph.cypher(
        "CREATE (a:N {id: 1}), (b:N {id: 2}), (c:N {id: 3}), "
        "(a)-[:R {text: 'alpha'}]->(b), (b)-[:R {text: 'beta'}]->(c)"
    )
    with pytest.raises(kglite.CypherExecutionError, match="deleted or replaced earlier in this statement"):
        graph.cypher(
            "MATCH p = (a:N)-[:R*2..2]->(c:N) WITH p "
            "MATCH (x:N)-[r:R]->(y:N) WHERE x.id = 1 DELETE r "
            "CREATE (x)-[:R {text: 'fresh'}]->(y) WITH p UNWIND relationships(p) AS pr "
            "CALL db.relationship_embeddings.set({type: 'R', text_property: 'text', "
            "entries: [{relationship: pr, vector: [1.0, 0.0]}]}) YIELD stored RETURN stored"
        )
