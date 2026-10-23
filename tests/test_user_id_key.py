"""A user property named `id` behaves like a key.

`id` is the node's identity, not a stored property. Drivers that treat it as an
ordinary key (GraphBridge's default key name, and its migration journal) need
three things from it, pinned here in every storage mode:

1. `REQUIRE n.id IS UNIQUE` is a real declaration, enforced on CREATE, listed in
   SHOW CONSTRAINTS, backed by an index SHOW INDEXES reports as owned.
2. `SET n.id = v` / `SET n += {id: v}` / `SET n = {id: v, ...}` is a no-op when
   `v` is the node's current id (the journal's `SET n.id = n.id` lock).
3. `CREATE (n:L) SET n = $props` takes the id out of `$props`.

Without the constraint nothing changes: a second `CREATE (:K {id: 'x'})` still
makes a second node.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pandas as pd
import pytest

import kglite

MODES = ("memory", "mapped", "disk")
IMMUTABLE = "immutable"


def _fresh(mode: str, tmp_path) -> kglite.KnowledgeGraph:
    if mode == "memory":
        return kglite.KnowledgeGraph()
    if mode == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))


def _rows(graph, query: str, **params) -> list[dict]:
    return graph.cypher(query, params=params or None).to_list()


def _count(graph, label: str) -> int:
    return _rows(graph, f"MATCH (n:{label}) RETURN count(n) AS c")[0]["c"]


def _ids(graph, label: str) -> list:
    return [r["i"] for r in _rows(graph, f"MATCH (n:{label}) RETURN n.id AS i ORDER BY i")]


def _reload(graph, mode: str, tmp_path) -> kglite.KnowledgeGraph:
    path = str(tmp_path / "g") if mode == "disk" else str(tmp_path / "g.kgl")
    graph.save(path)
    return kglite.load(path)


# ── 1. the declaration ───────────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_unique_on_id_is_declared_listed_and_enforced(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'x'})")
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    assert _rows(graph, "SHOW CONSTRAINTS YIELD name, type, labelsOrTypes, properties") == [
        {"name": "idk", "type": "UNIQUENESS", "labelsOrTypes": ["K"], "properties": ["id"]}
    ]
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:K {id: 'x'})")
    graph.cypher("CREATE (:K {id: 'y'})")
    assert _ids(graph, "K") == ["x", "y"]
    # MERGE is the upsert and still works.
    assert _rows(graph, "MERGE (c:K {id: 'x'}) ON CREATE SET c.v = 1 RETURN c.id AS i") == [{"i": "x"}]
    assert _count(graph, "K") == 2


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize(
    "statement",
    [
        "CREATE CONSTRAINT `gb_key` FOR (n:`K`) REQUIRE n.`id` IS UNIQUE",
        "CREATE CONSTRAINT `gb_key` FOR (n:`K`) REQUIRE (n.`id`) IS UNIQUE",
        "CREATE CONSTRAINT gb_key IF NOT EXISTS FOR (n:K) REQUIRE n.id IS UNIQUE",
        "CREATE CONSTRAINT gb_key FOR (n:K) REQUIRE n.id IS NODE KEY",
    ],
)
def test_the_graphbridge_spellings_declare_the_key(mode, statement, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher(statement)
    assert [r["name"] for r in _rows(graph, "SHOW CONSTRAINTS YIELD name")] == ["gb_key"]
    graph.cypher("CREATE (:K {id: 1})")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:K {id: 1})")


@pytest.mark.parametrize("mode", MODES)
def test_an_integer_id_collides_however_it_was_written(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.add_nodes(pd.DataFrame({"pid": [1, 2], "name": ["a", "b"]}), "K", "pid", "name")
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:K {id: 1})")
    graph.cypher("CREATE (:K {id: 3})")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:K {id: 3})")
    assert _count(graph, "K") == 3


@pytest.mark.parametrize("mode", MODES)
def test_the_constraint_is_refused_over_ids_that_already_collide(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'x'})")
    graph.cypher("CREATE (:K {id: 'x'})")
    with pytest.raises(Exception, match="duplicate|already"):
        graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    assert _rows(graph, "SHOW CONSTRAINTS") == []


@pytest.mark.parametrize("mode", MODES)
def test_the_constraint_is_refused_over_an_integer_id_written_two_ways(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.add_nodes(pd.DataFrame({"pid": [1], "name": ["a"]}), "K", "pid", "name")
    graph.cypher("CREATE (:K {id: 1})")
    assert _count(graph, "K") == 2
    with pytest.raises(Exception, match="duplicate|already"):
        graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")


@pytest.mark.parametrize("mode", MODES)
def test_without_the_constraint_duplicate_ids_are_still_made(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'x'})")
    graph.cypher("CREATE (:K {id: 'x'})")
    assert _count(graph, "K") == 2


@pytest.mark.parametrize("mode", MODES)
def test_dropping_the_constraint_lifts_enforcement(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    graph.cypher("CREATE (:K {id: 'x'})")
    graph.cypher("DROP CONSTRAINT idk")
    assert _rows(graph, "SHOW INDEXES") == []
    graph.cypher("CREATE (:K {id: 'x'})")
    assert _count(graph, "K") == 2


@pytest.mark.parametrize("mode", MODES)
def test_the_backing_range_index_is_listed_owned_online_and_undroppable(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT `gb_key` FOR (n:`K`) REQUIRE n.`id` IS UNIQUE")
    rows = _rows(
        graph,
        "SHOW INDEXES YIELD name, type, state, entityType, labelsOrTypes, properties, owningConstraint",
    )
    assert rows == [
        {
            "name": "gb_key",
            "type": "RANGE",
            "state": "ONLINE",
            "entityType": "NODE",
            "labelsOrTypes": ["K"],
            "properties": ["id"],
            "owningConstraint": "gb_key",
        }
    ]
    with pytest.raises(Exception, match="backs the constraint"):
        graph.cypher("DROP INDEX gb_key")
    # One namespace: an index may not take the constraint's name.
    with pytest.raises(Exception, match="already used"):
        graph.cypher("CREATE INDEX gb_key FOR (n:Other) ON (n.p)")


@pytest.mark.parametrize("mode", MODES)
def test_a_failed_statement_leaves_no_half_created_nodes(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:K {id: 1}), (:K {id: 2}), (:K {id: 1})")
    assert _count(graph, "K") == 0
    graph.cypher("CREATE (:K {id: 1})")
    assert _ids(graph, "K") == [1]


@pytest.mark.parametrize("mode", MODES)
def test_the_declaration_survives_save_and_load(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    graph.cypher("CREATE (:K {id: 'x'})")
    again = _reload(graph, mode, tmp_path)
    assert [r["name"] for r in _rows(again, "SHOW CONSTRAINTS YIELD name")] == ["idk"]
    assert [(r["name"], r["owningConstraint"]) for r in _rows(again, "SHOW INDEXES YIELD name, owningConstraint")] == [
        ("idk", "idk")
    ]
    with pytest.raises(kglite.ConstraintViolationError):
        again.cypher("CREATE (:K {id: 'x'})")


# ── 2. SET to the current id ─────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_setting_the_id_to_itself_is_a_no_op_in_every_spelling(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'a', x: 1, y: 2})")
    graph.cypher("MATCH (n:K {id: 'a'}) SET n.id = n.id")
    graph.cypher("MATCH (n:K {id: 'a'}) SET n.`id` = n.`id`")
    graph.cypher("MATCH (n:K {id: 'a'}) SET n += {id: 'a', x: 5}")
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x, n.y AS y") == [{"i": "a", "x": 5, "y": 2}]
    graph.cypher("MATCH (n:K {id: 'a'}) SET n = {id: 'a', z: 3}")
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x, n.z AS z") == [{"i": "a", "x": None, "z": 3}]
    assert _count(graph, "K") == 1


@pytest.mark.parametrize("mode", MODES)
def test_the_lock_statement_runs_inside_a_transaction(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'a', lock: 0})")
    with graph.begin() as tx:
        tx.cypher("MATCH (n:K {id: 'a'}) SET n.`id` = n.`id`")
        tx.cypher("MATCH (n:K {id: 'a'}) SET n.lock = n.lock + 1")
    assert _rows(graph, "MATCH (n:K) RETURN n.lock AS l") == [{"l": 1}]


@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize(
    "statement",
    [
        "MATCH (n:K {id: 'a'}) SET n.id = 'b'",
        "MATCH (n:K {id: 'a'}) SET n += {id: 'b'}",
        "MATCH (n:K {id: 'a'}) SET n = {id: 'b', x: 1}",
    ],
)
def test_a_different_id_on_an_existing_node_is_refused(mode, statement, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'a', x: 0})")
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher(statement)
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x") == [{"i": "a", "x": 0}]


@pytest.mark.parametrize("mode", MODES)
def test_remove_id_stays_refused(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {id: 'a'})")
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("MATCH (n:K) REMOVE n.id")


@pytest.mark.parametrize("mode", MODES)
def test_the_merge_and_lock_statements_still_work(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT `gb_key` FOR (n:`__Ctl`) REQUIRE n.`id` IS UNIQUE")
    merge = (
        "MERGE (c:__Ctl {id: $id}) ON CREATE SET c.formatVersion = 1, c.generation = 0, c.lock = 0 RETURN c.id AS id"
    )
    assert _rows(graph, merge, id="database") == [{"id": "database"}]
    assert _rows(graph, merge, id="database") == [{"id": "database"}]
    graph.cypher("MATCH (c:__Ctl {id: $id}) SET c.lock = c.lock + 1", params={"id": "database"})
    assert _rows(graph, "MATCH (c:__Ctl) RETURN c.lock AS l, c.formatVersion AS f") == [{"l": 1, "f": 1}]
    assert _count(graph, "__Ctl") == 1


# ── 3. the id named in the SET after a CREATE ────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_create_then_set_map_takes_the_id_from_the_map(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (n:`K`) SET n = $properties", params={"properties": {"id": "a", "x": 1}})
    graph.cypher("CREATE (n:`K`) SET n += $changes", params={"changes": {"id": "b", "x": 2}})
    graph.cypher("CREATE (n:K) SET n.id = 'c', n.x = 3")
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x ORDER BY i") == [
        {"i": "a", "x": 1},
        {"i": "b", "x": 2},
        {"i": "c", "x": 3},
    ]
    # The node is found by its id, as any node created with an id is.
    assert _rows(graph, "MATCH (n:K {id: 'b'}) RETURN n.x AS x") == [{"x": 2}]


@pytest.mark.parametrize("mode", MODES)
def test_the_title_follows_the_new_id_like_a_created_node(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (n:K) SET n += $p", params={"p": {"id": "a"}})
    assert _rows(graph, "MATCH (n:K) RETURN n.title AS t") == [{"t": "K_a"}]


@pytest.mark.parametrize("mode", MODES)
def test_the_unique_constraint_is_checked_against_the_named_id(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    graph.cypher("CREATE (n:K) SET n = $p", params={"p": {"id": "a", "x": 1}})
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (n:K) SET n = $p", params={"p": {"id": "a", "x": 2}})
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x") == [{"i": "a", "x": 1}]


@pytest.mark.parametrize("mode", MODES)
def test_a_row_per_node_takes_its_own_id(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    rows = [{"id": i, "v": i * 10} for i in range(1, 6)]
    graph.cypher("UNWIND $rows AS r CREATE (n:K) SET n = r", params={"rows": rows})
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.v AS v ORDER BY i") == [
        {"i": r["id"], "v": r["v"]} for r in rows
    ]
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("UNWIND $rows AS r CREATE (n:K) SET n = r", params={"rows": [{"id": 9}, {"id": 3}]})
    assert _count(graph, "K") == 5  # the whole statement rolled back, id 9 with it


@pytest.mark.parametrize("mode", MODES)
def test_several_nodes_in_one_create_each_take_their_id(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (a:A)-[:R]->(b:B) SET a = $pa, b = $pb", params={"pa": {"id": 1}, "pb": {"id": "q"}})
    assert _rows(graph, "MATCH (a:A)-[:R]->(b:B) RETURN a.id AS a, b.id AS b") == [{"a": 1, "b": "q"}]


@pytest.mark.parametrize("mode", MODES)
def test_a_map_without_an_id_leaves_the_minted_one(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (n:K) SET n = $p", params={"p": {"x": 1}})
    row = _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x")[0]
    assert row["x"] == 1 and row["i"] is not None


@pytest.mark.parametrize("mode", MODES)
def test_the_pattern_may_name_the_same_id_the_map_does(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (n:K {id: 'a'}) SET n = $p", params={"p": {"id": "a", "x": 1}})
    assert _rows(graph, "MATCH (n:K) RETURN n.id AS i, n.x AS x") == [{"i": "a", "x": 1}]
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("CREATE (n:K {id: 'b'}) SET n = $p", params={"p": {"id": "c"}})
    assert _count(graph, "K") == 1


@pytest.mark.parametrize("mode", MODES)
def test_only_the_set_directly_after_a_create_names_its_ids(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    # The FOREACH body's `n` is a new node, not the one the first SET named.
    graph.cypher(
        "CREATE (n:K) SET n = $p WITH 1 AS x FOREACH (y IN [1] | CREATE (n:K))",
        params={"p": {"id": "named"}},
    )
    assert _count(graph, "K") == 2
    assert "named" in _ids(graph, "K")


@pytest.mark.parametrize("mode", MODES)
def test_a_node_from_an_earlier_statement_is_not_re_keyed(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE (:K {x: 1})")
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("MATCH (n:K) SET n = $p", params={"p": {"id": "new"}})
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("MATCH (n:K) SET n += $p", params={"p": {"id": "new"}})
    assert _rows(graph, "MATCH (n:K) RETURN n.x AS x") == [{"x": 1}]


@pytest.mark.parametrize("mode", MODES)
def test_an_id_that_is_not_known_before_the_create_is_refused(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("CREATE (n:K) CREATE (m:M) SET n = $p", params={"p": {"id": "late"}})
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("CREATE (n:K) SET n.id = randomUUID()")
    # Any function call is refused, deterministic or not: the SET re-evaluates it.
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("CREATE (n:K) SET n.id = toString(7)")
    assert _count(graph, "K") == 0 and _count(graph, "M") == 0


@pytest.mark.parametrize("mode", MODES)
def test_a_statement_failing_after_the_set_rolls_the_node_back(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    with pytest.raises(kglite.CypherExecutionError):
        graph.cypher("CREATE (n:K) SET n = $p, n.z = 1 / 0", params={"p": {"id": "gone"}})
    assert _count(graph, "K") == 0
    graph.cypher("CREATE (n:K) SET n = $p", params={"p": {"id": "gone"}})
    assert _ids(graph, "K") == ["gone"]


@pytest.mark.parametrize("mode", MODES)
def test_the_named_id_survives_save_and_load(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
    graph.cypher(
        "UNWIND $rows AS r CREATE (n:K) SET n = r", params={"rows": [{"id": "a", "v": 1}, {"id": "b", "v": 2}]}
    )
    again = _reload(graph, mode, tmp_path)
    assert _rows(again, "MATCH (n:K) RETURN n.id AS i, n.v AS v ORDER BY i") == [
        {"i": "a", "v": 1},
        {"i": "b", "v": 2},
    ]
    with pytest.raises(kglite.ConstraintViolationError):
        again.cypher("CREATE (n:K) SET n = $p", params={"p": {"id": "a"}})


@pytest.mark.parametrize("storage", ["memory", "mapped"])
def test_the_named_id_replays_from_the_write_ahead_log(storage, tmp_path):
    script = textwrap.dedent(
        f"""
        import kglite, os
        path = {str(tmp_path / "app.kgl")!r}
        kwargs = {{"durable": True}}
        if {storage!r} != "memory":
            kwargs["storage"] = {storage!r}
        g = kglite.open(path, **kwargs)
        g.cypher("CREATE CONSTRAINT idk FOR (n:K) REQUIRE n.id IS UNIQUE")
        g.cypher("CREATE (n:K) SET n = $p", params={{"p": {{"id": "a", "x": 1}}}})
        g.cypher("UNWIND $rows AS r CREATE (n:K) SET n = r", params={{"rows": [{{"id": "b"}}, {{"id": "c"}}]}})
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, timeout=60, env={**os.environ})
    graph = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert _ids(graph, "K") == ["a", "b", "c"]
    assert _rows(graph, "MATCH (n:K {id: 'a'}) RETURN n.x AS x") == [{"x": 1}]
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:K {id: 'b'})")


# ── an aliased id spelling ───────────────────────────────────────────


@pytest.mark.parametrize("mode", MODES)
def test_the_types_own_id_spelling_gets_the_same_treatment(mode, tmp_path):
    graph = _fresh(mode, tmp_path)
    graph.add_nodes(pd.DataFrame({"pid": [1, 2], "name": ["a", "b"]}), "P", "pid", "name")
    graph.cypher("CREATE CONSTRAINT pk FOR (n:P) REQUIRE n.pid IS UNIQUE")
    with pytest.raises(kglite.ConstraintViolationError):
        graph.cypher("CREATE (:P {pid: 2})")
    graph.cypher("MATCH (n:P {pid: 1}) SET n.pid = n.pid")
    with pytest.raises(kglite.CypherExecutionError, match=IMMUTABLE):
        graph.cypher("MATCH (n:P {pid: 1}) SET n.pid = 7")
    graph.cypher("CREATE (n:P) SET n = $p", params={"p": {"pid": 9}})
    assert _count(graph, "P") == 3
