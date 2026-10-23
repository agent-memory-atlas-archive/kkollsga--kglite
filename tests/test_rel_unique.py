"""Relationship ``UNIQUE`` constraints, through the Python surface.

``CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.p IS UNIQUE`` says no two
relationships of type ``T`` hold the same non-null value of ``p``. It owns a
relationship index (``SHOW CONSTRAINTS`` ``ownedIndex``, ``SHOW INDEXES``
``owningConstraint``) and is checked on every edge write path.

The Rust golden tests (``dir_graph/rel_unique_tests.rs``) pin the same rules
against the engine, including the value-family equality table. A disk graph
refuses the declaration, so the memory/mapped transcript is the parity case and
disk has its own refusal test.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

HEAP_MODES = ("memory", "mapped")
MODES = ("memory", "mapped", "disk")
UNIQUE_ERROR = "UNIQUE constraint"
DECLARE = "CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE"


def _fresh(mode: str, tmp_path) -> KnowledgeGraph:
    if mode == "memory":
        return KnowledgeGraph()
    if mode == "mapped":
        return KnowledgeGraph(storage="mapped")
    return KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))


def _seed(kg: KnowledgeGraph) -> None:
    kg.cypher("CREATE (:P {id: 1}), (:P {id: 2}), (:P {id: 3}), (:M {id: 10}), (:M {id: 11}), (:M {id: 12})")


def _link(kg: KnowledgeGraph, person: int, movie: int, k) -> None:
    kg.cypher(
        "MATCH (p:P {id: $p}), (m:M {id: $m}) CREATE (p)-[:T {k: $k}]->(m)",
        params={"p": person, "m": movie, "k": k},
    )


def _count(kg: KnowledgeGraph, query: str, **params) -> int:
    return kg.cypher(query, params=params or None).to_dicts()[0]["c"]


@pytest.fixture(params=HEAP_MODES)
def kg(request, tmp_path) -> KnowledgeGraph:
    graph = _fresh(request.param, tmp_path)
    _seed(graph)
    graph.cypher(DECLARE)
    return graph


# ---------------------------------------------------------------------------
# Declaration and listing
# ---------------------------------------------------------------------------


def test_both_listings_name_each_other(kg):
    _link(kg, 1, 10, "a")
    constraints = kg.cypher(
        "SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes, properties, ownedIndex "
        "RETURN name, type, entityType, labelsOrTypes, properties, ownedIndex"
    ).to_dicts()
    assert constraints == [
        {
            "name": "rk",
            "type": "RELATIONSHIP_UNIQUENESS",
            "entityType": "RELATIONSHIP",
            "labelsOrTypes": ["T"],
            "properties": ["k"],
            "ownedIndex": "relationship:T.k",
        }
    ]
    indexes = kg.cypher("SHOW INDEXES YIELD name, entityType, owningConstraint").to_dicts()
    assert indexes == [{"name": "relationship:T.k", "entityType": "RELATIONSHIP", "owningConstraint": "rk"}]


def test_declaring_over_repeated_values_is_a_creation_error_and_changes_nothing(tmp_path):
    kg = KnowledgeGraph()
    _seed(kg)
    _link(kg, 1, 10, "a")
    _link(kg, 1, 11, "a")
    with pytest.raises(kglite.ConstraintCreationError, match="duplicate"):
        kg.cypher("CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE")
    assert kg.cypher("SHOW CONSTRAINTS").to_list() == []
    assert kg.cypher("SHOW INDEXES").to_list() == []
    _link(kg, 2, 10, "a")  # nothing is enforced


def test_the_unsupported_shapes_say_why(kg):
    with pytest.raises(Exception, match="exactly one property"):
        kg.cypher("CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE (r.a, r.b) IS UNIQUE")
    with pytest.raises(Exception, match="IS UNIQUE.*IS NOT NULL"):
        kg.cypher("CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.z IS RELATIONSHIP KEY")
    with pytest.raises(Exception, match="already exists"):
        kg.cypher("CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE")
    kg.cypher("CREATE CONSTRAINT IF NOT EXISTS FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE")


def test_a_disk_graph_refuses_the_declaration_and_says_why(tmp_path):
    kg = _fresh("disk", tmp_path)
    _seed(kg)
    with pytest.raises(Exception, match="storage='disk'"):
        kg.cypher(DECLARE)


def test_a_type_with_a_validity_interval_is_refused_both_ways():
    kg = KnowledgeGraph()
    kg.cypher(
        "CREATE (:P {id: 1})-[:V {k: 'a', valid_from: date('2020-01-01'), valid_to: date('2020-12-31')}]->(:M {id: 10})"
    )
    kg.cypher("CALL db.temporal.declare({relationship: 'V', from: 'valid_from', to: 'valid_to', convention: 'closed'})")
    with pytest.raises(Exception, match="validity interval"):
        kg.cypher("CREATE CONSTRAINT FOR ()-[r:V]-() REQUIRE r.k IS UNIQUE")
    other = KnowledgeGraph()
    other.cypher(
        "CREATE (:P {id: 1})-[:V {k: 'a', valid_from: date('2020-01-01'), valid_to: date('2020-12-31')}]->(:M {id: 10})"
    )
    other.cypher("CREATE CONSTRAINT FOR ()-[r:V]-() REQUIRE r.k IS UNIQUE")
    with pytest.raises(Exception, match="UNIQUE constraint"):
        other.cypher(
            "CALL db.temporal.declare({relationship: 'V', from: 'valid_from', to: 'valid_to', convention: 'closed'})"
        )


# ---------------------------------------------------------------------------
# Enforcement
# ---------------------------------------------------------------------------


def test_every_cypher_write_route_enforces_with_the_typed_error(kg):
    _link(kg, 1, 10, "a")
    _link(kg, 2, 10, "b")
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR) as raised:
        _link(kg, 3, 11, "a")
    assert "relationship of type 'T'" in str(raised.value)
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
        kg.cypher("MATCH (p:P {id: 3}), (m:M {id: 11}) UNWIND [1, 2] AS i CREATE (p)-[:T {k: 'same'}]->(m)")
    assert _count(kg, "MATCH ()-[r:T]->() RETURN count(r) AS c") == 2
    for statement in (
        "MATCH (:P {id: 2})-[r:T]->() SET r.k = 'a'",
        "MATCH (:P {id: 2})-[r:T]->() SET r += {k: 'a'}",
        "MATCH (:P {id: 2})-[r:T]->() SET r = {k: 'a'}",
    ):
        with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
            kg.cypher(statement)
    kg.cypher("MATCH (:P {id: 1})-[r:T]->() SET r.k = 'a'")  # its own value
    kg.cypher("MATCH (p:P {id: 1}), (m:M {id: 10}) MERGE (p)-[r:T {k: 'a'}]->(m)")  # hits
    assert _count(kg, "MATCH ()-[r:T]->() RETURN count(r) AS c") == 2
    kg.cypher("MATCH (:P {id: 1})-[r:T]->() REMOVE r.k")
    _link(kg, 3, 11, "a")
    with pytest.raises(kglite.ConstraintViolationError):
        _link(kg, 3, 12, "a")


def test_null_values_claim_nothing_and_parallel_edges_may_differ(kg):
    for _ in range(2):
        kg.cypher("MATCH (p:P {id: 1}), (m:M {id: 10}) CREATE (p)-[:T]->(m)")
    kg.cypher("MATCH (p:P {id: 1}), (m:M {id: 10}) CREATE (p)-[:T {k: null}]->(m)")
    _link(kg, 2, 10, "x")
    _link(kg, 2, 10, "y")
    with pytest.raises(kglite.ConstraintViolationError):
        _link(kg, 2, 10, "x")
    assert _count(kg, "MATCH ()-[r:T]->() RETURN count(r) AS c") == 5


def test_numeric_spellings_are_distinct_values(kg):
    _link(kg, 1, 10, 1)
    _link(kg, 2, 10, 1.0)
    _link(kg, 3, 10, "1")
    with pytest.raises(kglite.ConstraintViolationError):
        _link(kg, 1, 11, 1)
    with pytest.raises(kglite.ConstraintViolationError):
        _link(kg, 1, 11, 1.0)


def test_the_bulk_loader_judges_the_state_a_frame_leaves(kg):
    kg.add_nodes(pd.DataFrame({"id": [20, 21]}), "M", "id", "id")
    _link(kg, 1, 10, "a")
    # Against a stored edge of another pair, and twice in one frame.
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
        kg.add_relationships(pd.DataFrame({"p": [2], "m": [10], "k": ["a"]}), "T", "P", "p", "M", "m")
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
        kg.add_relationships(pd.DataFrame({"p": [2, 3], "m": [20, 21], "k": ["n", "n"]}), "T", "P", "p", "M", "m")
    assert _count(kg, "MATCH ()-[r:T]->() RETURN count(r) AS c") == 1
    # A legal frame lands; an update that merges into the pair's own edge is not
    # a conflict with itself, and one that moves a value frees it for another.
    kg.add_relationships(pd.DataFrame({"p": [2, 3], "m": [20, 21], "k": ["b", "c"]}), "T", "P", "p", "M", "m")
    kg.add_relationships(
        pd.DataFrame({"p": [1, 2], "m": [10, 20], "k": ["a", "z"]}),
        "T",
        "P",
        "p",
        "M",
        "m",
        conflict_handling="update",
    )
    kg.add_relationships(
        pd.DataFrame({"p": [1, 3], "m": [10, 12], "k": ["moved", "a"]}),
        "T",
        "P",
        "p",
        "M",
        "m",
        conflict_handling="update",
    )
    assert _count(kg, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r) AS c") == 1
    assert _count(kg, "MATCH (:P {id: 1})-[r:T {k: 'moved'}]->() RETURN count(r) AS c") == 1
    # `replace_relationships` deletes the sources' edges first, so a frame may
    # reuse the values it deletes.
    kg.replace_relationships(pd.DataFrame({"p": [1], "m": [10], "k": ["moved"]}), "T", "P", "p", "M", "m")
    assert _count(kg, "MATCH ()-[r:T {k: 'moved'}]->() RETURN count(r) AS c") == 1
    with pytest.raises(kglite.ConstraintViolationError):
        kg.replace_relationships(pd.DataFrame({"p": [1], "m": [10], "k": ["z"]}), "T", "P", "p", "M", "m")
    assert _count(kg, "MATCH ()-[r:T {k: 'moved'}]->() RETURN count(r) AS c") == 1, "a refused frame deletes nothing"


def test_a_failed_statement_and_a_rolled_back_transaction_give_claims_back(kg):
    _link(kg, 1, 10, "a")
    _link(kg, 2, 10, "b")
    with pytest.raises(Exception):
        kg.cypher(
            "MATCH (:P {id: 1})-[r:T]->() SET r.k = 'moved' "
            "WITH r MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'moved'}]->(m)"
        )
    with pytest.raises(kglite.ConstraintViolationError):
        _link(kg, 3, 11, "a")
    _link(kg, 3, 11, "moved")
    kg.cypher("MATCH ()-[r:T {k: 'moved'}]->() DELETE r")

    tx = kg.begin()
    tx.cypher("MATCH ()-[r:T {k: 'a'}]->() DELETE r")
    tx.cypher("MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'a'}]->(m)")
    tx.rollback()
    with pytest.raises(kglite.ConstraintViolationError):
        _link(kg, 3, 12, "a")
    with kg.begin() as tx:
        tx.cypher("MATCH ()-[r:T {k: 'a'}]->() DELETE r")
    _link(kg, 3, 12, "a")


def test_deletes_release_claims_and_vacuum_keeps_them(kg):
    _link(kg, 1, 10, "a")
    _link(kg, 2, 10, "b")
    kg.cypher("MATCH ()-[r:T {k: 'a'}]->() DELETE r")
    _link(kg, 3, 11, "a")
    kg.cypher("MATCH (p:P {id: 2}) DETACH DELETE p")
    _link(kg, 3, 10, "b")
    kg.vacuum()
    for held in ("a", "b"):
        with pytest.raises(kglite.ConstraintViolationError):
            _link(kg, 1, 12, held)
    _link(kg, 1, 12, "fresh")


# ---------------------------------------------------------------------------
# Ownership
# ---------------------------------------------------------------------------


def test_drop_index_is_refused_and_drop_constraint_removes_the_index(kg):
    for statement in (
        "DROP INDEX `relationship:T.k`",
        "DROP INDEX `relationship:T.k` IF EXISTS",
        "DROP INDEX FOR ()-[r:T]-() ON (r.k)",
    ):
        with pytest.raises(Exception, match="backs the constraint 'rk'"):
            kg.cypher(statement)
    with pytest.raises(Exception, match="already exists"):
        kg.cypher("CREATE INDEX FOR ()-[r:T]-() ON (r.k)")
    kg.cypher("CREATE INDEX IF NOT EXISTS FOR ()-[r:T]-() ON (r.k)")
    kg.cypher("DROP CONSTRAINT rk")
    assert kg.cypher("SHOW INDEXES").to_list() == []
    _link(kg, 1, 10, "x")
    _link(kg, 2, 10, "x")


def test_an_index_declared_first_is_adopted_and_outlives_the_constraint(tmp_path):
    kg = KnowledgeGraph()
    _seed(kg)
    _link(kg, 1, 10, "a")
    kg.cypher("CREATE INDEX FOR ()-[r:T]-() ON (r.k)")
    kg.cypher(DECLARE)
    assert len(kg.cypher("SHOW INDEXES").to_list()) == 1
    with pytest.raises(Exception, match="backs the constraint"):
        kg.cypher("DROP INDEX FOR ()-[r:T]-() ON (r.k)")
    kg.cypher("DROP CONSTRAINT rk")
    assert [row["name"] for row in kg.cypher("SHOW INDEXES YIELD name").to_dicts()] == ["relationship:T.k"]
    kg.cypher("DROP INDEX FOR ()-[r:T]-() ON (r.k)")


def test_the_owned_index_serves_the_identified_edge_read(kg):
    _link(kg, 1, 10, "a")
    assert "RelIndexSeek :T(k)" in " | ".join(
        row["operation"] for row in kg.cypher("EXPLAIN MATCH ()-[r:T {k: 'a'}]->() RETURN r").to_list()
    )
    assert _count(kg, "MATCH (p)-[r:T {k: 'a'}]->(m) RETURN count(r) AS c") == 1


# ---------------------------------------------------------------------------
# Persistence
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", HEAP_MODES)
def test_a_saved_graph_reloads_enforcing(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    _seed(kg)
    kg.cypher(DECLARE)
    _link(kg, 1, 10, "a")
    path = str(tmp_path / "g.kgl")
    kg.save(path)
    loaded = kglite.load(path)
    assert [row["name"] for row in loaded.cypher("SHOW CONSTRAINTS YIELD name").to_dicts()] == ["rk"]
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
        _link(loaded, 2, 10, "a")
    loaded.cypher("DROP CONSTRAINT rk")
    _link(loaded, 2, 10, "a")


def _crash_child(tmp_path, body: str) -> None:
    script = textwrap.dedent(
        f"""
        import kglite, os
        path = {str(tmp_path / "app.kgl")!r}
        g = kglite.open(path, durable=True)
        {textwrap.indent(textwrap.dedent(body), "        ").strip()}
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, env=dict(os.environ))


def test_a_declaration_survives_a_crash_before_any_checkpoint(tmp_path):
    _crash_child(
        tmp_path,
        """
g.cypher("CREATE (:P {id: 1}), (:P {id: 2}), (:M {id: 10})")
g.cypher("MATCH (p:P {id: 1}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a'}]->(m)")
g.cypher("CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE")
g.cypher("MATCH (p:P {id: 2}), (m:M {id: 10}) CREATE (p)-[:T {k: 'b'}]->(m)")
""",
    )
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    assert [row["name"] for row in g.cypher("SHOW CONSTRAINTS YIELD name").to_dicts()] == ["rk"]
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
        _link(g, 2, 10, "a")
    with pytest.raises(kglite.ConstraintViolationError, match=UNIQUE_ERROR):
        _link(g, 1, 10, "b")


# ---------------------------------------------------------------------------
# Parity
# ---------------------------------------------------------------------------

TRANSCRIPT = [
    ("dup_create", "refused"),
    ("dup_set", "refused"),
    ("own_value", "admitted"),
    ("after_delete", "admitted"),
    ("after_remove", "admitted"),
    ("count", 4),
]


def _outcome(kg, statement, **params) -> str:
    try:
        kg.cypher(statement, params=params or None)
    except Exception as error:
        assert UNIQUE_ERROR in str(error), error
        return "refused"
    return "admitted"


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_the_unique_transcript_is_identical_in_every_storage_mode(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    _seed(kg)
    try:
        kg.cypher(DECLARE)
        declared = True
    except Exception as error:
        assert "storage='disk'" in str(error), error
        declared = False
    assert declared == (mode != "disk")
    if not declared:
        return  # a disk graph refuses the declaration, so there is nothing to enforce
    _link(kg, 1, 10, "a")
    _link(kg, 2, 10, "b")
    got = [
        (
            "dup_create",
            _outcome(kg, "MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'a'}]->(m)"),
        ),
        ("dup_set", _outcome(kg, "MATCH (:P {id: 2})-[r:T]->() SET r.k = 'a'")),
        ("own_value", _outcome(kg, "MATCH (:P {id: 1})-[r:T]->() SET r.k = 'a'")),
    ]
    kg.cypher("MATCH ()-[r:T {k: 'b'}]->() DELETE r")
    got.append(
        (
            "after_delete",
            _outcome(kg, "MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'b'}]->(m)"),
        )
    )
    kg.cypher("MATCH (:P {id: 1})-[r:T]->() REMOVE r.k")
    got.append(
        (
            "after_remove",
            _outcome(kg, "MATCH (p:P {id: 3}), (m:M {id: 12}) CREATE (p)-[:T {k: 'a'}]->(m)"),
        )
    )
    kg.cypher("MATCH (p:P {id: 2}), (m:M {id: 12}) CREATE (p)-[:T]->(m)")
    got.append(("count", _count(kg, "MATCH ()-[r:T]->() RETURN count(r) AS c")))
    assert got == TRANSCRIPT
