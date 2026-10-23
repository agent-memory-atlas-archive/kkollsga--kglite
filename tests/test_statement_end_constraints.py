"""NOT NULL / NODE KEY are judged when a Cypher statement ends, and every
constraint kind on one tuple keeps its own `SHOW CONSTRAINTS` name.

Neo4j checks existence and key constraints on the statement's final state, so
`CREATE (n:L) SET n = $props` is valid on a type that requires a property
`$props` supplies. kglite used to judge presence inside `CREATE`, before the
`SET` ran, which made that pattern (the one GraphBridge emits for every entity)
fail on any model declaring an existence constraint or a NODE KEY.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite

STORAGES = ["memory", "mapped", "disk"]


def _graph(storage: str, tmp_path) -> kglite.KnowledgeGraph:
    if storage == "memory":
        graph = kglite.KnowledgeGraph()
    elif storage == "mapped":
        graph = kglite.KnowledgeGraph(storage="mapped")
    else:
        graph = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "kg"))
    # Seed a type so the graph is non-empty on every backend.
    graph.add_nodes(pd.DataFrame({"sid": [1], "name": ["seed"]}), "Seed", "sid", "name")
    return graph


def _count(graph, label: str) -> int:
    return graph.cypher(f"MATCH (n:{label}) RETURN count(n) AS c").to_list()[0]["c"]


# ── nodes ────────────────────────────────────────────────────────────


@pytest.mark.parametrize("storage", STORAGES)
def test_create_then_set_map_satisfies_not_null(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS NOT NULL")
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.tag IS NOT NULL")
    graph.cypher("CREATE (n:N) SET n = $p", params={"p": {"pid": 1, "tag": "a"}})
    row = graph.cypher("MATCH (n:N) RETURN n.pid AS pid, n.tag AS tag").to_list()
    assert row == [{"pid": 1, "tag": "a"}]


@pytest.mark.parametrize("storage", STORAGES)
def test_create_then_set_map_satisfies_node_key(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT k FOR (n:N) REQUIRE n.pid IS NODE KEY")
    graph.cypher("CREATE (n:N) SET n = $p", params={"p": {"pid": 1}})
    # The uniqueness half is still judged per write.
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (n:N) SET n = $p", params={"p": {"pid": 1}})
    assert _count(graph, "N") == 1


@pytest.mark.parametrize("storage", STORAGES)
def test_a_statement_that_violates_and_then_repairs_itself_passes(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS NOT NULL")
    graph.cypher("CREATE (n:N {pid: 1})")
    graph.cypher("MATCH (n:N) REMOVE n.pid SET n.pid = 7")
    graph.cypher("MATCH (n:N) SET n.pid = null SET n.pid = 8")
    assert graph.cypher("MATCH (n:N) RETURN n.pid AS pid").to_list() == [{"pid": 8}]


@pytest.mark.parametrize("storage", STORAGES)
def test_a_statement_that_ends_in_violation_rolls_back_entirely(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS NOT NULL")
    graph.cypher("CREATE (n:N {pid: 1})")
    with pytest.raises(kglite.ConstraintViolationError, match="must have the property"):
        graph.cypher("CREATE (a:N {pid: 2}) CREATE (b:N) CREATE (c:Other {x: 1})")
    assert _count(graph, "N") == 1
    assert _count(graph, "Other") == 0
    # A lone single-node CREATE leaves nothing behind either (it loses the
    # checkpoint-free fast path on a type that requires a property).
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (n:N)")
    assert _count(graph, "N") == 1
    # The edit to an existing node rolls back with the rest of its statement.
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("MATCH (n:N) SET n.extra = 5 REMOVE n.pid")
    assert graph.cypher("MATCH (n:N) RETURN n.pid AS pid, n.extra AS extra").to_list() == [{"pid": 1, "extra": None}]


@pytest.mark.parametrize("storage", STORAGES)
def test_remove_and_set_null_of_a_required_property_fail(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS NOT NULL")
    graph.cypher("CREATE (n:N {pid: 1})")
    for statement in ("MATCH (n:N) REMOVE n.pid", "MATCH (n:N) SET n.pid = null"):
        with pytest.raises(kglite.ConstraintViolationError, match="must have the property"):
            graph.cypher(statement)
        assert graph.cypher("MATCH (n:N) RETURN n.pid AS pid").to_list() == [{"pid": 1}]


@pytest.mark.parametrize("storage", STORAGES)
def test_merge_creating_a_node_without_the_property_fails(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS NOT NULL")
    graph.cypher("MERGE (n:N {pid: 1})")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("MERGE (n:N {pid: 2}) ON CREATE SET n.pid = null")
    assert _count(graph, "N") == 1


@pytest.mark.parametrize("storage", STORAGES)
def test_the_bulk_loader_still_judges_each_complete_row(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS NOT NULL")
    frame = pd.DataFrame({"nid": [1, 2], "pid": [5, None]})
    with pytest.raises(Exception, match="must have the property"):
        graph.add_nodes(frame, "N", "nid", "nid")
    assert _count(graph, "N") == 0


@pytest.mark.parametrize("storage", STORAGES)
def test_a_wrongly_typed_value_is_still_refused_when_written(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT FOR (n:N) REQUIRE n.pid IS :: INTEGER")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (n:N) SET n.pid = 'x'")
    assert _count(graph, "N") == 0


# ── relationships ────────────────────────────────────────────────────


def _two_nodes(graph):
    graph.cypher("CREATE (:Endp {i: 1}), (:Endp {i: 2})")


@pytest.mark.parametrize("storage", STORAGES)
def test_relationship_create_then_set_map_satisfies_not_null(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    _two_nodes(graph)
    graph.cypher("CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.rid IS NOT NULL")
    graph.cypher(
        "MATCH (a:Endp {i: 1}), (b:Endp {i: 2}) CREATE (a)-[r:T]->(b) SET r = $p",
        params={"p": {"rid": 3}},
    )
    assert graph.cypher("MATCH ()-[r:T]->() RETURN r.rid AS rid").to_list() == [{"rid": 3}]


@pytest.mark.parametrize("storage", STORAGES)
def test_relationship_statement_ending_in_violation_rolls_back(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    _two_nodes(graph)
    graph.cypher("CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.rid IS NOT NULL")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("MATCH (a:Endp {i: 1}), (b:Endp {i: 2}) CREATE (a)-[r:T {rid: 1}]->(b), (b)-[q:T]->(a)")
    assert graph.cypher("MATCH ()-[r:T]->() RETURN count(r) AS c").to_list() == [{"c": 0}]
    graph.cypher("MATCH (a:Endp {i: 1}), (b:Endp {i: 2}) CREATE (a)-[r:T {rid: 1}]->(b)")
    for statement in ("MATCH ()-[r:T]->() REMOVE r.rid", "MATCH ()-[r:T]->() SET r.rid = null"):
        with pytest.raises(kglite.ConstraintViolationError):
            graph.cypher(statement)
    assert graph.cypher("MATCH ()-[r:T]->() RETURN r.rid AS rid").to_list() == [{"rid": 1}]
    graph.cypher("MATCH ()-[r:T]->() REMOVE r.rid SET r.rid = 4")
    assert graph.cypher("MATCH ()-[r:T]->() RETURN r.rid AS rid").to_list() == [{"rid": 4}]


# ── F15: SHOW CONSTRAINTS naming ─────────────────────────────────────


def _rows(graph):
    return {r["name"]: r for r in graph.cypher("SHOW CONSTRAINTS").to_list()}


@pytest.mark.parametrize("storage", STORAGES)
def test_every_kind_on_one_tuple_keeps_its_own_name(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    graph.cypher("CREATE CONSTRAINT n1 FOR (n:N) REQUIRE n.p IS NOT NULL")
    graph.cypher("CREATE CONSTRAINT n2 FOR (n:N) REQUIRE n.p IS :: INTEGER")
    graph.cypher("CREATE CONSTRAINT n3 FOR (n:N) REQUIRE n.p IS UNIQUE")
    rows = _rows(graph)
    assert {name: row["type"] for name, row in rows.items()} == {
        "n1": "NODE_PROPERTY_EXISTENCE",
        "n2": "NODE_PROPERTY_TYPE",
        "n3": "UNIQUENESS",
    }
    graph.cypher("CREATE CONSTRAINT r1 FOR ()-[r:T]-() REQUIRE r.p IS NOT NULL")
    graph.cypher("CREATE CONSTRAINT r2 FOR ()-[r:T]-() REQUIRE r.p IS :: INTEGER")
    rel = {n: r["type"] for n, r in _rows(graph).items() if n.startswith("r")}
    assert rel == {"r1": "RELATIONSHIP_PROPERTY_EXISTENCE", "r2": "RELATIONSHIP_PROPERTY_TYPE"}
    # Each name drops exactly its own constraint.
    for name in ("n2", "n1", "n3", "r2", "r1"):
        graph.cypher(f"DROP CONSTRAINT {name}")
        assert name not in _rows(graph)
    assert _rows(graph) == {}


def test_a_declared_node_key_is_still_one_row():
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE CONSTRAINT k FOR (n:N) REQUIRE n.p IS NODE KEY")
    rows = _rows(graph)
    assert {name: row["type"] for name, row in rows.items()} == {"k": "NODE_KEY"}
