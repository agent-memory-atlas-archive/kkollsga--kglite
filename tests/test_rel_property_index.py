"""Relationship property indexes, through the Python surface.

``CREATE INDEX FOR ()-[r:T]-() ON (r.p)`` builds an equality index over one
property of one relationship type, and a pattern whose first hop constrains
that property starts from the index instead of scanning every node.

The oracle is the scan: every probe is answered twice, once through the index
(the inline map form always seeds from it) and once as a ``WHERE`` with the
optimizer disabled, which never pushes the predicate into the pattern. After
every kind of write the two must agree, so a write path that forgets to
maintain the index shows up as a missing row. The Rust golden tests
(``dir_graph/rel_index_tests.rs``) pin the same rules against the engine and
compare the maintained index with one rebuilt from the edges.

A disk graph refuses the declaration (its relationship ids are a frozen CSR
plus a process-local overlay), so the memory/mapped transcript is the parity
case and disk has its own refusal test.
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
PROBES = ["a", "a2", "b", "c", "x", 1, 1.0, "1", "absent", "new"]


def _fresh(mode: str, tmp_path) -> KnowledgeGraph:
    if mode == "memory":
        return KnowledgeGraph()
    if mode == "mapped":
        return KnowledgeGraph(storage="mapped")
    return KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))


def _seed(kg: KnowledgeGraph) -> None:
    kg.cypher(
        "CREATE (:P {id: 1, pid: 1}), (:P {id: 2, pid: 2}), (:P {id: 3, pid: 3}), "
        "(:M {id: 10, pid: 10}), (:M {id: 11, pid: 11}), (:M {id: 12, pid: 12})"
    ).to_list()
    for person, movie, rid in [(1, 10, "a"), (2, 10, "b"), (3, 11, "c"), (1, 11, 1), (2, 11, 1.0), (3, 10, "1")]:
        kg.cypher(
            "MATCH (p:P {pid: $p}), (m:M {pid: $m}) CREATE (p)-[:ACTED {rid: $rid, w: $p}]->(m)",
            params={"p": person, "m": movie, "rid": rid},
        ).to_list()


def _indexed(kg: KnowledgeGraph, probe) -> list[tuple]:
    rows = kg.cypher("MATCH (p)-[r:ACTED {rid: $k}]->(m) RETURN p.pid AS p, m.pid AS m", params={"k": probe}).to_list()
    return sorted((row["p"], row["m"]) for row in rows)


def _scanned(kg: KnowledgeGraph, probe) -> list[tuple]:
    rows = kg.cypher(
        "MATCH (p)-[r:ACTED]->(m) WHERE r.rid = $k RETURN p.pid AS p, m.pid AS m",
        params={"k": probe},
        disable_optimizer=True,
    ).to_list()
    return sorted((row["p"], row["m"]) for row in rows)


def _assert_agrees(kg: KnowledgeGraph) -> None:
    for probe in PROBES:
        assert _indexed(kg, probe) == _scanned(kg, probe), f"probe {probe!r}"


def _plan(kg: KnowledgeGraph, query: str) -> str:
    return " | ".join(row["operation"] for row in kg.cypher(f"EXPLAIN {query}").to_list())


@pytest.fixture(params=HEAP_MODES)
def kg(request, tmp_path) -> KnowledgeGraph:
    graph = _fresh(request.param, tmp_path)
    _seed(graph)
    graph.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
    return graph


# ---------------------------------------------------------------------------
# DDL and listing
# ---------------------------------------------------------------------------


def test_the_index_is_listed_with_its_entity_and_dropped_by_either_spelling(kg):
    rows = kg.cypher(
        "SHOW INDEXES YIELD name, entityType, labelsOrTypes, properties, state, owningConstraint "
        "WHERE entityType = 'RELATIONSHIP' RETURN name, labelsOrTypes, properties, state, owningConstraint"
    ).to_dicts()
    assert rows == [
        {
            "name": "relationship:ACTED.rid",
            "labelsOrTypes": ["ACTED"],
            "properties": ["rid"],
            "state": "ONLINE",
            "owningConstraint": None,
        }
    ]
    with pytest.raises(Exception, match="already exists"):
        kg.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
    kg.cypher("CREATE INDEX IF NOT EXISTS FOR ()-[r:ACTED]-() ON (r.rid)")
    kg.cypher("DROP INDEX `relationship:ACTED.rid`")
    assert (
        kg.cypher("SHOW INDEXES YIELD entityType WHERE entityType = 'RELATIONSHIP' RETURN entityType").to_list() == []
    )
    kg.cypher("CREATE RANGE INDEX FOR ()-[r:ACTED]-() ON (r.w)")
    kg.cypher("DROP INDEX FOR ()-[r:ACTED]-() ON (r.w)")
    with pytest.raises(Exception, match="no index"):
        kg.cypher("DROP INDEX FOR ()-[r:ACTED]-() ON (r.w)")


def test_a_composite_relationship_index_is_refused(kg):
    with pytest.raises(Exception, match="exactly one property"):
        kg.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid, r.w)")


def test_a_disk_graph_refuses_the_declaration_and_says_why(tmp_path):
    kg = _fresh("disk", tmp_path)
    _seed(kg)
    with pytest.raises(Exception, match="storage='disk'"):
        kg.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
    # The unindexed query still works.
    assert _scanned(kg, "a") == [(1, 10)]


# ---------------------------------------------------------------------------
# The anchor
# ---------------------------------------------------------------------------


def test_explain_shows_the_seek_for_inline_and_where_forms(kg):
    assert "RelIndexSeek :ACTED(rid)" in _plan(kg, "MATCH ()-[r:ACTED {rid: 'a'}]->() RETURN r")
    where = _plan(kg, "MATCH ()-[r:ACTED]->() WHERE r.rid = 'a' RETURN r")
    assert "RelIndexSeek :ACTED(rid)" in where and "OptimizerPass anchor_rel_property_index" in where
    assert "RelIndexSeek" not in _plan(kg, "MATCH ()-[r:ACTED {w: 1}]->() RETURN r")
    assert "RelIndexSeek" not in _plan(kg, "MATCH ()-[r]->() WHERE r.rid = 'a' RETURN r")


def test_the_index_and_the_scan_agree_on_every_probe_family(kg):
    _assert_agrees(kg)


def test_a_correlated_call_reads_each_row_through_the_index(kg):
    query = (
        "MATCH (:P)-[r:ACTED]->(:M) WITH r CALL (r) { MATCH ()-[c:ACTED]->() WHERE c.rid = r.rid "
        "WITH c LIMIT 2 RETURN count(*) AS n } RETURN r.rid AS k, n ORDER BY k, n"
    )
    indexed = kg.cypher(query).to_list()
    kg.cypher("DROP INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
    assert kg.cypher(query).to_list() == indexed
    # Numeric spellings of one value count together: 1 and 1.0 find each other.
    assert [row["n"] for row in indexed if row["k"] in (1, 1.0)] == [2, 2]


# ---------------------------------------------------------------------------
# Every write path keeps the index exact
# ---------------------------------------------------------------------------


def test_cypher_writes(kg):
    kg.cypher("MATCH (p:P {pid: 3}), (m:M {pid: 12}) CREATE (p)-[:ACTED {rid: 'new', w: 99}]->(m)")
    _assert_agrees(kg)
    kg.cypher("MATCH (p:P {pid: 2}), (m:M {pid: 12}) MERGE (p)-[r:ACTED {rid: 'x'}]->(m)")
    kg.cypher("MATCH (p:P {pid: 2}), (m:M {pid: 12}) MERGE (p)-[r:ACTED {rid: 'x'}]->(m)")
    assert len(_indexed(kg, "x")) == 1
    _assert_agrees(kg)
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = 'a2'")
    _assert_agrees(kg)
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a2'}]->() SET r += {rid: 'a3', extra: 1}")
    assert _indexed(kg, "a3") == [(1, 10)]
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a3'}]->() SET r = {rid: 'a4'}")
    assert _indexed(kg, "a4") == [(1, 10)]
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a4'}]->() REMOVE r.rid")
    assert _indexed(kg, "a4") == []
    kg.cypher("MATCH (p:P {pid: 1})-[r:ACTED]->(:M {pid: 10}) SET r.rid = 'a'")
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = null")
    assert _indexed(kg, "a") == []
    kg.cypher("MATCH ()-[r:ACTED {rid: 'c'}]->() DELETE r")
    _assert_agrees(kg)
    kg.cypher("MATCH (p:P {pid: 2}) DETACH DELETE p")
    _assert_agrees(kg)
    assert _indexed(kg, "b") == []
    # A freed edge slot is handed to a new edge with another value.
    kg.cypher("MATCH (p:P {pid: 3}), (m:M {pid: 10}) CREATE (p)-[:ACTED {rid: 'reused'}]->(m)")
    assert _indexed(kg, "reused") == [(3, 10)]


def test_the_bulk_loader_in_every_conflict_mode(kg):
    frame = pd.DataFrame({"p": [1, 2], "m": [12, 12], "rid": ["bulk1", "bulk2"]})
    kg.add_relationships(frame, "ACTED", "P", "p", "M", "m")
    assert _indexed(kg, "bulk1") == [(1, 12)]
    # `update` merges into the stored edge between the same endpoints and moves
    # its indexed value.
    update = pd.DataFrame({"p": [1], "m": [12], "rid": ["bulk1-moved"]})
    kg.add_relationships(update, "ACTED", "P", "p", "M", "m", conflict_handling="update")
    assert _indexed(kg, "bulk1") == [] and _indexed(kg, "bulk1-moved") == [(1, 12)]
    # `preserve` keeps the stored value.
    preserve = pd.DataFrame({"p": [1], "m": [12], "rid": ["ignored"]})
    kg.add_relationships(preserve, "ACTED", "P", "p", "M", "m", conflict_handling="preserve")
    assert _indexed(kg, "ignored") == [] and _indexed(kg, "bulk1-moved") == [(1, 12)]
    # `replace` swaps the edge for a fresh one.
    replace = pd.DataFrame({"p": [2], "m": [12], "rid": ["bulk2-replaced"]})
    kg.add_relationships(replace, "ACTED", "P", "p", "M", "m", conflict_handling="replace")
    assert _indexed(kg, "bulk2") == [] and _indexed(kg, "bulk2-replaced") == [(2, 12)]
    # `replace_relationships` deletes the sources' edges of the type first.
    fresh = pd.DataFrame({"p": [3], "m": [12], "rid": ["swept"]})
    kg.replace_relationships(fresh, "ACTED", "P", "p", "M", "m")
    assert _indexed(kg, "c") == [] and _indexed(kg, "swept") == [(3, 12)]
    _assert_agrees(kg)


def test_vacuum_renumbers_and_the_index_follows(kg):
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() DELETE r")
    kg.cypher("MATCH (p:P {pid: 1}) DETACH DELETE p")
    kg.vacuum()
    _assert_agrees(kg)
    assert _indexed(kg, "b") == [(2, 10)]


def test_a_transaction_rollback_restores_the_index_and_a_commit_keeps_it(kg):
    tx = kg.begin()
    tx.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = 'moved'")
    tx.cypher("MATCH ()-[r:ACTED {rid: 'b'}]->() DELETE r")
    tx.cypher("MATCH (p:P {pid: 3}), (m:M {pid: 12}) CREATE (p)-[:ACTED {rid: 'new'}]->(m)")
    tx.rollback()
    _assert_agrees(kg)
    assert _indexed(kg, "a") == [(1, 10)] and _indexed(kg, "new") == []
    with kg.begin() as tx:
        tx.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = 'a2'")
        assert tx.cypher("MATCH ()-[r:ACTED {rid: 'a2'}]->() RETURN count(r) AS c").to_dicts() == [{"c": 1}]
    _assert_agrees(kg)
    assert _indexed(kg, "a2") == [(1, 10)]


def test_a_failed_statement_is_atomic_for_the_index(kg):
    with pytest.raises(Exception):
        kg.cypher(
            "MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = 'moved' WITH 1 AS x UNWIND [1, 0] AS d RETURN 1 / d"
        ).to_list()
    _assert_agrees(kg)
    assert _indexed(kg, "a") == [(1, 10)] and _indexed(kg, "moved") == []


# ---------------------------------------------------------------------------
# Persistence
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("mode", HEAP_MODES)
def test_a_saved_graph_reloads_with_the_index_built(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    _seed(kg)
    kg.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
    path = str(tmp_path / "g.kgl")
    kg.save(path)
    loaded = kglite.load(path)
    names = [
        row["name"]
        for row in loaded.cypher(
            "SHOW INDEXES YIELD name, entityType WHERE entityType = 'RELATIONSHIP' RETURN name"
        ).to_dicts()
    ]
    assert names == ["relationship:ACTED.rid"]
    _assert_agrees(loaded)
    assert "RelIndexSeek" in _plan(loaded, "MATCH ()-[r:ACTED {rid: 'a'}]->() RETURN r")
    loaded.cypher("MATCH (p:P {pid: 3}), (m:M {pid: 12}) CREATE (p)-[:ACTED {rid: 'new'}]->(m)")
    _assert_agrees(loaded)


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


def test_a_declaration_and_later_writes_survive_a_crash_before_any_checkpoint(tmp_path):
    _crash_child(
        tmp_path,
        """
g.cypher("CREATE (:P {pid: 1}), (:P {pid: 2}), (:M {pid: 10})")
g.cypher("MATCH (p:P {pid: 1}), (m:M {pid: 10}) CREATE (p)-[:ACTED {rid: 'a'}]->(m)")
g.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
g.cypher("MATCH (p:P {pid: 2}), (m:M {pid: 10}) CREATE (p)-[:ACTED {rid: 'b'}]->(m)")
g.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = 'a2'")
""",
    )
    wal = (tmp_path / "app.kgl-wal").read_bytes()
    assert wal[:5] == b"KWAL\x0d", "a log carrying a relationship index declaration is WAL format 13"
    g = kglite.open(str(tmp_path / "app.kgl"), durable=True)
    names = [
        row["name"]
        for row in g.cypher(
            "SHOW INDEXES YIELD name, entityType WHERE entityType = 'RELATIONSHIP' RETURN name"
        ).to_dicts()
    ]
    assert names == ["relationship:ACTED.rid"]
    assert _indexed(g, "a2") == [(1, 10)] and _indexed(g, "b") == [(2, 10)] and _indexed(g, "a") == []
    _assert_agrees(g)


# ---------------------------------------------------------------------------
# Parity
# ---------------------------------------------------------------------------

MODES = ("memory", "mapped", "disk")

# The answers a probe gives are the same in every storage mode; only whether
# the index exists differs (a disk graph refuses it and scans).
PROBE_TRANSCRIPT = [
    ("before", "'a'", [(1, 10)]),
    ("before", "'a2'", []),
    ("before", "'b'", [(2, 10)]),
    ("before", "'c'", [(3, 11)]),
    ("before", "1", [(1, 11), (2, 11)]),
    ("before", "1.0", [(1, 11), (2, 11)]),
    ("before", "'1'", [(3, 10)]),
    ("before", "'new'", []),
    ("after", "'a'", []),
    ("after", "'a2'", [(1, 10)]),
    ("after", "'b'", []),
    ("after", "'c'", [(3, 11)]),
    ("after", "'new'", [(3, 12)]),
]


@pytest.mark.parity
@pytest.mark.parametrize("mode", MODES)
def test_the_rel_index_transcript_is_identical_in_every_storage_mode(mode, tmp_path):
    kg = _fresh(mode, tmp_path)
    _seed(kg)
    try:
        kg.cypher("CREATE INDEX FOR ()-[r:ACTED]-() ON (r.rid)")
        ddl = "built"
    except Exception as error:
        assert "storage='disk'" in str(error), error
        ddl = "refused"
    assert ddl == ("refused" if mode == "disk" else "built")
    probes = {"'a'": "a", "'a2'": "a2", "'b'": "b", "'c'": "c", "1": 1, "1.0": 1.0, "'1'": "1", "'new'": "new"}
    got = [("before", label, _indexed(kg, value)) for label, value in probes.items()]
    kg.cypher("MATCH ()-[r:ACTED {rid: 'a'}]->() SET r.rid = 'a2'")
    kg.cypher("MATCH ()-[r:ACTED {rid: 'b'}]->() DELETE r")
    kg.cypher("MATCH (p:P {pid: 3}), (m:M {pid: 12}) CREATE (p)-[:ACTED {rid: 'new'}]->(m)")
    got += [("after", label, _indexed(kg, probes[label])) for label in ("'a'", "'a2'", "'b'", "'c'", "'new'")]
    assert got == PROBE_TRANSCRIPT
    plan = _plan(kg, "MATCH ()-[r:ACTED]->() WHERE r.rid = 'a2' RETURN r")
    assert ("RelIndexSeek" in plan) == (mode != "disk")
