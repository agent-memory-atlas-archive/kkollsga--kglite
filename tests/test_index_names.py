"""User-chosen index names: `CREATE INDEX <name>`, `DROP INDEX <name>`, the shared
index/constraint namespace, the index a constraint owns, and persistence.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import textwrap

import pytest

import kglite
from kglite import KnowledgeGraph

HEAP = ["memory", "mapped"]  # a disk graph's property indexes are persistent bundles SHOW INDEXES does not list


def _graph(mode: str = "memory") -> KnowledgeGraph:
    graph = KnowledgeGraph(storage=mode)
    graph.cypher("CREATE (:L {a: 1, b: 2, text: 'quick fox'}), (:L {a: 2, b: 3, text: 'lazy dog'})")
    return graph


def _rows(graph: KnowledgeGraph, query: str = "SHOW INDEXES", **params) -> list[dict]:
    return graph.cypher(query, params=params or None).to_list()


def _names(graph: KnowledgeGraph) -> list[tuple[str, str]]:
    return sorted((r["name"], r["type"]) for r in _rows(graph))


def _roundtrip(graph: KnowledgeGraph, tmp_path: Path) -> KnowledgeGraph:
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    return kglite.load(path)


# ── CREATE INDEX <name> / DROP INDEX <name> ──────────────────────────


@pytest.mark.parametrize("mode", HEAP)
def test_a_created_name_is_listed_and_drops_the_index(mode):
    graph = _graph(mode)
    graph.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
    assert _names(graph) == [("ix_a", "PROPERTY")]
    graph.cypher("DROP INDEX ix_a")
    assert _names(graph) == []
    assert not graph.has_index("L", "a")


@pytest.mark.parametrize("mode", HEAP)
def test_the_canonical_name_still_drops_a_named_index(mode):
    graph = _graph(mode)
    graph.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
    graph.cypher("DROP INDEX `L.a`")
    assert _names(graph) == []
    # The name went with the index: a later index on the property is unnamed.
    graph.cypher("CREATE INDEX FOR (n:L) ON (n.a)")
    assert _names(graph) == [("L.a", "PROPERTY")]


@pytest.mark.parametrize("mode", HEAP)
def test_a_range_name_covers_both_structures_and_drops_both(mode):
    graph = _graph(mode)
    graph.cypher("CREATE RANGE INDEX rg FOR (n:L) ON (n.a)")
    assert _names(graph) == [("rg", "PROPERTY"), ("rg", "RANGE")]
    graph.cypher("DROP INDEX rg")
    assert _names(graph) == []
    assert not graph.has_index("L", "a")


@pytest.mark.parametrize("mode", HEAP)
def test_a_composite_index_takes_a_name(mode):
    graph = _graph(mode)
    graph.cypher("CREATE INDEX cx FOR (n:L) ON (n.a, n.b)")
    assert _names(graph) == [("cx", "PROPERTY")]
    graph.cypher("DROP INDEX cx")
    assert _names(graph) == []


def test_dropping_an_unknown_name_still_errors_and_if_exists_is_a_no_op():
    graph = _graph()
    graph.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
    with pytest.raises(Exception, match="no index named 'nope'"):
        graph.cypher("DROP INDEX nope")
    graph.cypher("DROP INDEX nope IF EXISTS")
    assert _names(graph) == [("ix_a", "PROPERTY")]


# ── one namespace ────────────────────────────────────────────────────


def test_an_index_name_cannot_reuse_a_constraint_name():
    graph = _graph()
    graph.cypher("CREATE CONSTRAINT shared FOR (n:L) REQUIRE n.b IS UNIQUE")
    with pytest.raises(Exception, match="constraint"):
        graph.cypher("CREATE INDEX shared FOR (n:L) ON (n.a)")
    graph.cypher("CREATE INDEX shared IF NOT EXISTS FOR (n:L) ON (n.a)")  # Neo4j: a taken name is a no-op
    assert not graph.has_index("L", "a")


def test_a_constraint_name_cannot_reuse_an_index_name():
    graph = _graph()
    graph.cypher("CREATE INDEX shared FOR (n:L) ON (n.a)")
    with pytest.raises(Exception, match="index already has that name"):
        graph.cypher("CREATE CONSTRAINT shared FOR (n:L) REQUIRE n.b IS UNIQUE")
    with pytest.raises(Exception, match="index already has that name"):
        graph.cypher("CREATE CONSTRAINT shared FOR ()-[r:T]-() REQUIRE r.x IS NOT NULL")
    assert _rows(graph, "SHOW CONSTRAINTS") == []


def test_an_index_name_cannot_reuse_another_indexes_name_or_canonical_name():
    graph = _graph()
    graph.cypher("CREATE INDEX first FOR (n:L) ON (n.a)")
    with pytest.raises(Exception, match="already exists"):
        graph.cypher("CREATE INDEX first FOR (n:L) ON (n.b)")
    with pytest.raises(Exception, match="canonical name"):
        graph.cypher("CREATE INDEX `L.a` FOR (n:L) ON (n.b)")
    graph.cypher("CREATE INDEX first IF NOT EXISTS FOR (n:L) ON (n.b)")
    assert not graph.has_index("L", "b")
    assert _names(graph) == [("first", "PROPERTY")]


# ── the index a constraint owns ──────────────────────────────────────


def test_an_owned_index_is_named_after_its_constraint():
    graph = _graph()
    graph.cypher("CREATE CONSTRAINT uq_a FOR (n:L) REQUIRE n.a IS UNIQUE")
    graph.cypher("CREATE CONSTRAINT FOR (n:L) REQUIRE n.b IS UNIQUE")  # unnamed: canonical stays
    rows = {r["name"]: r["owningConstraint"] for r in _rows(graph, "SHOW INDEXES YIELD name, owningConstraint")}
    assert rows == {"uq_a": "uq_a", "L.b": "L.b"}
    owned = {r["name"]: r["ownedIndex"] for r in _rows(graph, "SHOW CONSTRAINTS YIELD name, ownedIndex")}
    assert owned == {"uq_a": "uq_a", "L.b": "L.b"}
    for spelling in ("uq_a", "`L.a`"):
        with pytest.raises(Exception, match="backs the constraint"):
            graph.cypher(f"DROP INDEX {spelling}")
    graph.cypher("DROP CONSTRAINT uq_a")
    assert [r["name"] for r in _rows(graph)] == ["L.b"]


def test_an_adopted_index_keeps_its_own_name():
    graph = _graph()
    graph.cypher("CREATE INDEX mine FOR (n:L) ON (n.a)")
    graph.cypher("CREATE CONSTRAINT uq_a FOR (n:L) REQUIRE n.a IS UNIQUE")
    assert [(r["name"], r["owningConstraint"]) for r in _rows(graph, "SHOW INDEXES YIELD name, owningConstraint")] == [
        ("mine", "uq_a")
    ]
    graph.cypher("DROP CONSTRAINT uq_a")
    assert _names(graph) == [("mine", "PROPERTY")]  # it survives, and keeps the name


# ── persistence ──────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", HEAP)
def test_names_survive_save_and_load(mode, tmp_path):
    graph = _graph(mode)
    graph.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
    graph.cypher("CREATE RANGE INDEX rg_b FOR (n:L) ON (n.b)")
    before = _names(graph)
    assert before == [("ix_a", "PROPERTY"), ("rg_b", "PROPERTY"), ("rg_b", "RANGE")]
    reloaded = _roundtrip(graph, tmp_path)
    assert _names(reloaded) == before
    reloaded.cypher("DROP INDEX ix_a")
    assert _names(reloaded) == [("rg_b", "PROPERTY"), ("rg_b", "RANGE")]


def test_a_name_does_not_outlive_its_index_into_a_later_one(tmp_path):
    graph = _graph()
    graph.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
    graph.drop_index("L", "a")  # not through DROP INDEX
    graph.create_index("L", "a")
    assert _names(graph) == [("L.a", "PROPERTY")]
    assert _names(_roundtrip(graph, tmp_path)) == [("L.a", "PROPERTY")]


def test_an_unnamed_graph_writes_no_name_field(tmp_path):
    graph = _graph()
    graph.cypher("CREATE INDEX FOR (n:L) ON (n.a)")
    unnamed = tmp_path / "unnamed.kgl"
    graph.save(str(unnamed))
    graph.cypher("CREATE INDEX ix_b FOR (n:L) ON (n.b)")
    named = tmp_path / "named.kgl"
    graph.save(str(named))
    # The metadata section is plain JSON: the field is there when names are, and
    # absent otherwise, which keeps an unnamed graph's bytes what they were.
    assert b'"index_names"' in named.read_bytes()
    assert b"index_names" not in unnamed.read_bytes()


def test_a_file_with_names_opens_in_the_previous_release(tmp_path):
    """The field is additive: 0.21.0 ignores it, and a 0.21.0 file loads with no names.

    Needs the published 0.21.0 wheel in its own venv: `KGLITE_021_PYTHON=<python>`.
    """
    old = os.environ.get("KGLITE_021_PYTHON")
    if not old:
        pytest.skip("set KGLITE_021_PYTHON to a Python with kglite==0.21.0 installed")
    named = str(tmp_path / "named.kgl")
    graph = _graph()
    graph.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
    graph.cypher("CREATE CONSTRAINT uq_b FOR (n:L) REQUIRE n.b IS UNIQUE")
    graph.save(named)
    script = textwrap.dedent(
        """
        import sys, kglite
        assert kglite.__version__ == "0.21.0", kglite.__version__
        g = kglite.load(sys.argv[1])
        rows = sorted(r["name"] for r in g.cypher("SHOW INDEXES").to_list())
        assert rows == ["L.a"], rows  # 0.21.0 has no constraint-owned index, and ignores the names
        g.cypher("DROP INDEX `L.a`")
        g.save(sys.argv[2])
        old = kglite.KnowledgeGraph()
        old.cypher("CREATE (:L {a: 1})")
        old.cypher("CREATE INDEX FOR (n:L) ON (n.a)")
        old.save(sys.argv[3])
        """
    )
    resaved, legacy = str(tmp_path / "resaved.kgl"), str(tmp_path / "legacy.kgl")
    subprocess.run([old, "-I", "-c", script, named, resaved, legacy], check=True, cwd=str(tmp_path))
    assert [r["name"] for r in _rows(kglite.load(legacy))] == ["L.a"]
    again = kglite.load(resaved)
    assert [r["name"] for r in _rows(again)] == ["uq_b"]  # the owned index, named after its constraint
