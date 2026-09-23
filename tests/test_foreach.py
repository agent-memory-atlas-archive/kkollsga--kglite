"""FOREACH (var IN list | <update clauses>) — mutation control-flow.

Runs the body's update clauses once per list element with the loop
variable bound. A side-effect loop: the surrounding rows are unchanged.
0.12 Tier 2.
"""

import pytest

import kglite


def test_foreach_standalone_create():
    g = kglite.KnowledgeGraph()
    g.cypher("FOREACH (x IN [1, 2, 3] | CREATE (:N {id: x}))")
    ids = sorted(r["id"] for r in g.cypher("MATCH (n:N) RETURN n.id AS id").to_list())
    assert ids == [1, 2, 3]


def test_foreach_param_list_of_dicts():
    g = kglite.KnowledgeGraph()
    g.cypher(
        "FOREACH (r IN $rows | CREATE (:P {id: r.id, name: r.name}))",
        params={"rows": [{"id": 1, "name": "A"}, {"id": 2, "name": "B"}]},
    )
    names = [r["n"] for r in g.cypher("MATCH (p:P) RETURN p.name AS n ORDER BY p.id").to_list()]
    assert names == ["A", "B"]


def test_foreach_set_per_matched_row():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Q {id: 1}), (:Q {id: 2})")
    g.cypher("MATCH (q:Q) FOREACH (_ IN [1] | SET q.touched = true)")
    assert g.cypher("MATCH (q:Q) WHERE q.touched = true RETURN count(q) AS c")[0]["c"] == 2


def test_foreach_over_node_property_list():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Acc {id: 1, items: [10, 20, 30]})")
    g.cypher("MATCH (a:Acc) FOREACH (i IN a.items | CREATE (:Item {v: i}))")
    vs = sorted(r["v"] for r in g.cypher("MATCH (i:Item) RETURN i.v AS v").to_list())
    assert vs == [10, 20, 30]


def test_foreach_nested():
    g = kglite.KnowledgeGraph()
    g.cypher("FOREACH (x IN [1, 2] | FOREACH (y IN [10, 20] | CREATE (:Pair {x: x, y: y})))")
    assert g.cypher("MATCH (p:Pair) RETURN count(p) AS c")[0]["c"] == 4


def test_foreach_over_null_is_noop():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Acc {id: 1})")  # no `items` property → null
    g.cypher("MATCH (a:Acc) FOREACH (i IN a.items | CREATE (:Item {v: i}))")
    assert g.cypher("MATCH (i:Item) RETURN count(i) AS c")[0]["c"] == 0


def test_foreach_empty_list_is_noop():
    g = kglite.KnowledgeGraph()
    g.cypher("FOREACH (x IN [] | CREATE (:N {id: x}))")
    assert g.cypher("MATCH (n:N) RETURN count(n) AS c")[0]["c"] == 0


def test_foreach_delete_in_body():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:Tmp {id: 1}), (:Tmp {id: 2}), (:Keep {id: 3})")
    g.cypher("MATCH (t:Tmp) FOREACH (_ IN [1] | DELETE t)")
    assert g.cypher("MATCH (t:Tmp) RETURN count(t) AS c")[0]["c"] == 0
    assert g.cypher("MATCH (k:Keep) RETURN count(k) AS c")[0]["c"] == 1


def test_foreach_detach_delete_collected_list():
    """Regression (0.12.3): DETACH DELETE inside FOREACH over a *collected*
    list. The loop var binds a materialised node value (`Value::Node` in
    `projected`), not a MATCH-bound node — `execute_delete` only resolved
    `NodeRef`, so this was a silent no-op (the dedup idiom deleted nothing)."""
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:T {id: 1}), (:T {id: 2}), (:T {id: 3})")
    # Keep-first dedup idiom: delete every element past the head of a collected list.
    g.cypher("MATCH (t:T) WITH collect(t) AS ns FOREACH (e IN ns[1..] | DETACH DELETE e)")
    assert g.cypher("MATCH (t:T) RETURN count(t) AS c")[0]["c"] == 1
    # Whole-list variant removes the rest.
    g.cypher("MATCH (t:T) WITH collect(t) AS ns FOREACH (e IN ns | DETACH DELETE e)")
    assert g.cypher("MATCH (t:T) RETURN count(t) AS c")[0]["c"] == 0


def test_foreach_detach_delete_collected_detaches_edges():
    """DETACH inside FOREACH over a collected list must drop incident edges too."""
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (a:N {id: 1})-[:R]->(b:N {id: 2})")
    g.cypher("MATCH (n:N) WITH collect(n) AS ns FOREACH (e IN ns | DETACH DELETE e)")
    assert g.cypher("MATCH (n:N) RETURN count(n) AS c")[0]["c"] == 0
    assert g.cypher("MATCH ()-[r:R]->() RETURN count(r) AS c")[0]["c"] == 0


def test_foreach_non_list_errors():
    g = kglite.KnowledgeGraph()
    with pytest.raises(Exception):
        g.cypher("FOREACH (x IN 42 | CREATE (:N {id: x}))")


def test_foreach_body_rejects_read_clause():
    g = kglite.KnowledgeGraph()
    # A FOREACH body may only contain update clauses; MATCH is rejected at parse.
    with pytest.raises(Exception):
        g.cypher("FOREACH (x IN [1] | MATCH (n) RETURN n)")


# ── the loop variable IS the collected entity ────────────────────────────


def test_foreach_create_over_collected_nodes_reuses_the_node():
    """`CREATE (x)-[:S]->(x)` over `collect(a)` loops the relationship on `a`.

    The loop variable reaches CREATE as a projected node value, not a binding,
    and the clause used to treat that as an unbound name: one anonymous,
    label-less node per element, with the relationship looped on it.
    """
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:N {id: 1}), (:N {id: 2})")
    g.cypher(
        "MATCH (a:N) WHERE a.id = 1 WITH collect(a) AS roots FOREACH (x IN roots | CREATE (x)-[:S {tag: 'made'}]->(x))"
    )
    assert g.last_mutation_stats["nodes_created"] == 0
    assert g.last_mutation_stats["relationships_created"] == 1
    assert g.cypher("MATCH (n) RETURN count(n) AS c")[0]["c"] == 2
    assert g.cypher("MATCH (n)-[:S]->(m) RETURN n.id AS s, m.id AS t").to_list() == [{"s": 1, "t": 1}]


def test_foreach_set_over_collected_relationships_writes_each():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (a:N {id: 1}), (b:N {id: 2}), (a)-[:E {k: 0}]->(b), (a)-[:E {k: 1}]->(b)")
    g.cypher("MATCH ()-[e:E]->() WITH collect(e) AS es FOREACH (r IN es | SET r.hits = 1)")
    assert g.last_mutation_stats["properties_set"] == 2
    assert g.cypher("MATCH ()-[r:E]->() RETURN r.k AS k, r.hits AS hits ORDER BY k").to_list() == [
        {"k": 0, "hits": 1},
        {"k": 1, "hits": 1},
    ]


def test_foreach_over_a_deleted_collected_node_is_refused_not_recreated():
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:N {id: 1}), (:N {id: 2})")
    with pytest.raises(kglite.CypherExecutionError, match="no longer exists"):
        g.cypher(
            "MATCH (a:N) WHERE a.id = 1 WITH collect(a) AS ns "
            "FOREACH (x IN ns | DELETE x) FOREACH (x IN ns | CREATE (x)-[:S]->(:M))"
        )
    assert g.cypher("MATCH (n) RETURN count(n) AS c")[0]["c"] == 2, "the failed statement changed nothing"
