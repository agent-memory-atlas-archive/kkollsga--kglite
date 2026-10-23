"""The index a UNIQUE / NODE KEY constraint owns, through the Python surface.

A constraint has no structure of its own: it owns a hash (single property) or
composite index. These tests pin what that means to a caller, in every storage
mode: enforcement on every write route, the index serving lookups and following
writes, ``SHOW INDEXES`` / ``SHOW CONSTRAINTS`` naming both ends, the DDL rules
(``DROP INDEX`` refused, ``CREATE INDEX`` a duplicate, adoption of an index the
user declared), rollback, delete, and a save/load round trip.

The Rust golden tests (``dir_graph/owned_index_tests.rs``) pin the same rules
against the engine, including the value-family equality table.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite
from kglite import KnowledgeGraph

MODES = ("memory", "mapped", "disk")
UNIQUE_ERROR = "UNIQUE constraint"


def _fresh(mode: str, tmp_path) -> KnowledgeGraph:
    if mode == "memory":
        return KnowledgeGraph()
    if mode == "mapped":
        return KnowledgeGraph(storage="mapped")
    return KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))


def _count(kg, query: str, **params) -> int:
    return kg.cypher(query, params=params or None).to_dicts()[0]["c"]


def _index_rows(kg) -> list[tuple]:
    rows = kg.cypher(
        "SHOW INDEXES YIELD name, type, owningConstraint RETURN name, type, owningConstraint ORDER BY name, type"
    ).to_dicts()
    return [(r["name"], r["type"], r["owningConstraint"]) for r in rows]


def _save_reload(kg, mode: str, tmp_path) -> KnowledgeGraph:
    path = str(tmp_path / "g") if mode == "disk" else str(tmp_path / "g.kgl")
    kg.save(path)
    return kglite.load(path)


# A disk graph's own `CREATE INDEX` builds a persistent bundle that SHOW INDEXES
# does not list and that is separate from the heap index a constraint owns, so
# the two tests that mix user indexes with constraints cover memory and mapped.
HEAP_MODES = ("memory", "mapped")


@pytest.mark.parametrize("mode", HEAP_MODES)
def test_the_constraint_and_its_index_name_each_other(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.b, n.a) IS NODE KEY")
    kg.cypher("CREATE INDEX FOR (n:T) ON (n.plain)")
    # An owned index is named after its constraint, as Neo4j names it.
    assert _index_rows(kg) == [
        ("T.plain", "PROPERTY", None),
        ("composite", "PROPERTY", "composite"),
        ("single", "PROPERTY", "single"),
    ]
    owned = {r["name"]: r["ownedIndex"] for r in kg.cypher("SHOW CONSTRAINTS YIELD name, ownedIndex").to_dicts()}
    assert owned == {"single": "single", "composite": "composite"}
    assert kg.has_index("T", "k") and kg.has_composite_index("T", ["a", "b"])
    assert {(i["node_type"], i["property"]) for i in kg.list_indexes()} >= {("T", "k"), ("T", "plain")}


@pytest.mark.parametrize("mode", MODES)
def test_the_owned_index_serves_lookups_and_follows_every_write(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("UNWIND range(1, 200) AS i CREATE (:T {id: i, k: 'k' + toString(i)})")
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    assert _count(kg, "MATCH (n:T {k: 'k42'}) RETURN count(n) AS c") == 1
    assert _count(kg, "MATCH (n:T {k: 'nope'}) RETURN count(n) AS c") == 0
    kg.cypher("MATCH (n:T {k: 'k42'}) SET n.k = 'moved'")
    assert _count(kg, "MATCH (n:T {k: 'k42'}) RETURN count(n) AS c") == 0
    assert _count(kg, "MATCH (n:T {k: 'moved'}) RETURN count(n) AS c") == 1
    kg.cypher("MATCH (n:T {k: 'moved'}) DETACH DELETE n")
    assert _count(kg, "MATCH (n:T {k: 'moved'}) RETURN count(n) AS c") == 0
    kg.cypher("CREATE (:T {id: 500, k: 'fresh'})")
    assert _count(kg, "MATCH (n:T {k: 'fresh'}) RETURN count(n) AS c") == 1
    assert _count(kg, "MATCH (n:T) RETURN count(n) AS c") == 200


@pytest.mark.parametrize("mode", MODES)
def test_drop_index_is_refused_and_drop_constraint_removes_the_index(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE")
    for statement in ("DROP INDEX `T.k`", "DROP INDEX `T.k` IF EXISTS", "DROP INDEX `T.(a,b)`"):
        with pytest.raises(Exception, match="backs the constraint"):
            kg.cypher(statement)
    with pytest.raises(ValueError, match="'single'"):
        kg.drop_index("T", "k")
    with pytest.raises(ValueError, match="backs the constraint"):
        kg.drop_composite_index("T", ["b", "a"])
    assert len(_index_rows(kg)) == 2
    kg.cypher("CREATE (:T {k: 'x', a: 1, b: 1})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {k: 'x'})")
    kg.cypher("DROP CONSTRAINT single")
    kg.cypher("DROP CONSTRAINT composite")
    assert _index_rows(kg) == []
    kg.cypher("CREATE (:T {k: 'x'})")


@pytest.mark.parametrize("mode", MODES)
def test_create_index_equivalent_to_an_owned_index_is_a_duplicate(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    with pytest.raises(Exception, match="already exists"):
        kg.cypher("CREATE INDEX FOR (n:T) ON (n.k)")
    kg.cypher("CREATE INDEX IF NOT EXISTS FOR (n:T) ON (n.k)")
    assert _index_rows(kg) == [("single", "PROPERTY", "single")]
    # A no-op adopts nothing: the index still goes with its constraint.
    kg.cypher("DROP CONSTRAINT single")
    assert _index_rows(kg) == []


@pytest.mark.parametrize("mode", HEAP_MODES)
def test_an_index_declared_first_is_adopted_and_outlives_the_constraint(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE (:T {k: 'a', a: 1, b: 2})")
    kg.cypher("CREATE INDEX FOR (n:T) ON (n.k)")
    kg.cypher("CREATE INDEX FOR (n:T) ON (n.a, n.b)")
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE")
    assert len(_index_rows(kg)) == 2, "one index per slot, not two"
    with pytest.raises(Exception, match="backs the constraint"):
        kg.cypher("DROP INDEX `T.k`")
    kg.cypher("DROP CONSTRAINT single")
    kg.cypher("DROP CONSTRAINT composite")
    assert _index_rows(kg) == [("T.(a,b)", "PROPERTY", None), ("T.k", "PROPERTY", None)]
    kg.cypher("DROP INDEX `T.k`")
    assert kg.has_composite_index("T", ["a", "b"]) and not kg.has_index("T", "k")


@pytest.mark.parametrize("mode", MODES)
def test_naming_the_index_through_the_api_adopts_it(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.create_index("T", "k")
    kg.cypher("DROP CONSTRAINT single")
    if mode != "disk":
        # A disk graph's `create_index` builds its own persistent bundle; the
        # owned heap entry is separate and goes with the constraint.
        assert kg.has_index("T", "k")
        assert _index_rows(kg) == [("T.k", "PROPERTY", None)]


# ---------------------------------------------------------------------------
# Enforcement through every route
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", MODES)
def test_every_write_route_enforces(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'b'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 3, k: 'a'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("UNWIND [1, 2] AS i CREATE (:T {id: 10 + i, k: 'c'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("MATCH (n:T {id: 2}) SET n.k = 'a'")
    kg.cypher("MATCH (n:T {id: 1}) SET n.k = 'a'")  # its own value
    kg.cypher("MERGE (n:T {k: 'a'}) ON MATCH SET n.hit = true")
    assert _count(kg, "MATCH (n:T) RETURN count(n) AS c") == 2
    kg.cypher("MATCH (n:T {id: 1}) REMOVE n.k")
    kg.cypher("CREATE (:T {id: 4, k: 'a'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 5, k: 'a'})")


@pytest.mark.parametrize("mode", MODES)
def test_the_bulk_loader_enforces_against_stored_rows_and_within_a_batch(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.add_nodes(pd.DataFrame({"id": [1, 2], "k": ["a", "b"]}), "T", "id", "id")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.add_nodes(pd.DataFrame({"id": [3], "k": ["a"]}), "T", "id", "id")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.add_nodes(pd.DataFrame({"id": [4, 5], "k": ["z", "z"]}), "T", "id", "id")
    assert _count(kg, "MATCH (n:T) RETURN count(n) AS c") == 2
    # An update that moves a value frees the tuple it vacated, and the lookup
    # follows it.
    kg.add_nodes(
        pd.DataFrame({"id": [1], "k": ["moved"]}),
        "T",
        "id",
        "id",
        conflict_handling="update",
    )
    assert _count(kg, "MATCH (n:T {k: 'moved'}) RETURN count(n) AS c") == 1
    assert _count(kg, "MATCH (n:T {k: 'a'}) RETURN count(n) AS c") == 0
    kg.add_nodes(pd.DataFrame({"id": [6], "k": ["a"]}), "T", "id", "id")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 7, k: 'a'})")


@pytest.mark.parametrize("mode", MODES)
def test_a_failed_statement_gives_every_claim_back(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'b'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 3, k: 'new'}) WITH 1 AS x CREATE (:T {id: 4, k: 'a'})")
    kg.cypher("CREATE (:T {id: 5, k: 'new'})")  # the rolled-back claim is free
    kg.cypher("MATCH (n:T {id: 5}) DETACH DELETE n")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("MATCH (n:T {id: 1}) SET n.k = 'moved' WITH n CREATE (:T {id: 6, k: 'moved'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 7, k: 'a'})")  # the old value is held again
    kg.cypher("CREATE (:T {id: 8, k: 'moved'})")  # and the new one is free
    assert _count(kg, "MATCH (n:T) RETURN count(n) AS c") == 3


@pytest.mark.parametrize("mode", MODES)
def test_a_delete_releases_its_claims(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'b'})")
    kg.cypher("MATCH (n:T {id: 1}) DETACH DELETE n")
    kg.cypher("CREATE (:T {id: 3, k: 'a'})")
    kg.cypher("MATCH (n:T {id: 2}) DETACH DELETE n CREATE (:T {id: 4, k: 'b'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 5, k: 'b'})")
    kg.cypher("MATCH (n:T) DETACH DELETE n")
    kg.cypher("CREATE (:T {id: 6, k: 'z'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 7, k: 'z'})")


def test_a_transaction_rollback_leaves_the_claims_alone(tmp_path):
    kg = KnowledgeGraph()
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a'})")
    tx = kg.begin()
    tx.cypher("CREATE (:T {id: 2, k: 'b'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        tx.cypher("CREATE (:T {id: 3, k: 'b'})")
    tx.rollback()
    tx = kg.begin()
    tx.cypher("CREATE (:T {id: 4, k: 'b'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        tx.cypher("CREATE (:T {id: 5, k: 'a'})")
    tx.commit()
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        kg.cypher("CREATE (:T {id: 6, k: 'b'})")
    assert _count(kg, "MATCH (n:T) RETURN count(n) AS c") == 2


# ---------------------------------------------------------------------------
# Persistence: the file records the constraint, not the derived index
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", MODES)
def test_a_reload_rebuilds_the_owned_index_and_keeps_enforcing(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a', a: 1, b: 2})")
    before = _index_rows(kg)
    reloaded = _save_reload(kg, mode, tmp_path)
    assert _index_rows(reloaded) == before
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        reloaded.cypher("CREATE (:T {id: 2, k: 'a'})")
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        reloaded.cypher("CREATE (:T {id: 3, a: 1, b: 2})")
    assert _count(reloaded, "MATCH (n:T {k: 'a'}) RETURN count(n) AS c") == 1
    # Still owned-only after the round trip: the constraint takes it away.
    reloaded.cypher("DROP CONSTRAINT single")
    assert not reloaded.has_index("T", "k")


@pytest.mark.parametrize("mode", ("memory", "mapped"))
def test_an_adopted_index_stays_a_user_index_through_a_reload(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE INDEX FOR (n:T) ON (n.k)")
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a'})")
    reloaded = _save_reload(kg, mode, tmp_path)
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        reloaded.cypher("CREATE (:T {id: 2, k: 'a'})")
    reloaded.cypher("DROP CONSTRAINT single")
    assert reloaded.has_index("T", "k")
    assert _index_rows(reloaded) == [("T.k", "PROPERTY", None)]


@pytest.mark.parametrize("mode", ("memory", "mapped"))
def test_a_deferred_load_lists_the_owned_index_and_enforces_on_the_first_write(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE (:T {id: 1, k: 'a'})")
    path = str(tmp_path / "d.kgl")
    kg.save(path)
    reloaded = kglite.load(path, defer_index_rebuild=True)
    assert _index_rows(reloaded) == [("single", "PROPERTY", "single")]
    with pytest.raises(Exception, match=UNIQUE_ERROR):
        reloaded.cypher("CREATE (:T {id: 2, k: 'a'})")
    assert _count(reloaded, "MATCH (n:T {k: 'a'}) RETURN count(n) AS c") == 1


# ---------------------------------------------------------------------------
# One transcript, three storage modes
# ---------------------------------------------------------------------------

TRANSCRIPT = [
    ("rows", [("ck", "PROPERTY", "ck"), ("uk", "PROPERTY", "uk")]),
    ("dup_create", "refused"),
    ("dup_set", "refused"),
    ("dup_composite", "refused"),
    ("lookup_k5", 1),
    ("lookup_after_move", [0, 1]),
    ("after_delete_reinsert", "admitted"),
    ("drop_index", "refused"),
    ("final_rows", [("ck", "PROPERTY", "ck")]),
]


def _outcome(kg, statement) -> str:
    try:
        kg.cypher(statement)
    except Exception as error:
        assert UNIQUE_ERROR in str(error) or "backs the constraint" in str(error), error
        return "refused"
    return "admitted"


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_the_owned_index_transcript_is_identical_in_every_storage_mode(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    kg.cypher("CREATE CONSTRAINT uk FOR (n:T) REQUIRE n.k IS UNIQUE")
    kg.cypher("CREATE CONSTRAINT ck FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE")
    kg.cypher("UNWIND range(1, 20) AS i CREATE (:T {id: i, k: 'k' + toString(i), a: i, b: i * 2})")
    got = [("rows", _index_rows(kg))]
    got.append(("dup_create", _outcome(kg, "CREATE (:T {id: 100, k: 'k3'})")))
    got.append(("dup_set", _outcome(kg, "MATCH (n:T {id: 4}) SET n.k = 'k3'")))
    got.append(("dup_composite", _outcome(kg, "CREATE (:T {id: 101, a: 2, b: 4})")))
    got.append(("lookup_k5", _count(kg, "MATCH (n:T {k: 'k5'}) RETURN count(n) AS c")))
    kg.cypher("MATCH (n:T {id: 5}) SET n.k = 'moved'")
    got.append(
        (
            "lookup_after_move",
            [
                _count(kg, "MATCH (n:T {k: 'k5'}) RETURN count(n) AS c"),
                _count(kg, "MATCH (n:T {k: 'moved'}) RETURN count(n) AS c"),
            ],
        )
    )
    kg.cypher("MATCH (n:T {id: 6}) DETACH DELETE n")
    got.append(("after_delete_reinsert", _outcome(kg, "CREATE (:T {id: 102, k: 'k6'})")))
    got.append(("drop_index", _outcome(kg, "DROP INDEX `T.k`")))
    kg.cypher("DROP CONSTRAINT uk")
    got.append(("final_rows", _index_rows(kg)))
    assert got == TRANSCRIPT
