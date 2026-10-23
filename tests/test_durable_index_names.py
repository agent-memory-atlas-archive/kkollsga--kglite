"""Index names are recorded in the write-ahead log.

A child registers or releases index names on a durable graph and dies with
``os._exit`` before any checkpoint, so only WAL replay can bring the registry
back. The reopened graph must list the same names, resolve them in
``DROP INDEX``, and drop the ones a later statement released.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pytest

import kglite

MODES = ("memory", "mapped")

SEED = """
g.cypher("CREATE (:L {a: 1, b: 2, text: 'quick fox'}), (:L {a: 2, b: 3, text: 'lazy dog'})")
"""


def _child(tmp_path, storage: str, body: str) -> None:
    script = textwrap.dedent(
        f"""
        import kglite, os
        path = {str(tmp_path / "app.kgl")!r}
        kwargs = {{"durable": True}}
        if {storage!r} != "memory" and not os.path.exists(path):
            kwargs["storage"] = {storage!r}
        g = kglite.open(path, **kwargs)
        {textwrap.indent(textwrap.dedent(body), "        ").strip()}
        os._exit(0)
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, env=dict(os.environ))


def _reopen(tmp_path):
    return kglite.open(str(tmp_path / "app.kgl"), durable=True)


def _names(g) -> list[tuple[str, str]]:
    rows = g.cypher("SHOW INDEXES").to_list()
    return sorted((r["name"], r["type"]) for r in rows)


@pytest.mark.parametrize("storage", MODES)
def test_a_named_create_index_survives_a_crash_and_drops_by_name(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
g.cypher("CREATE RANGE INDEX rg_b FOR (n:L) ON (n.b)")
g.cypher("CREATE INDEX cx FOR (n:L) ON (n.a, n.b)")
""",
    )
    assert not (tmp_path / "app.kgl").exists()
    g = _reopen(tmp_path)
    assert _names(g) == [
        ("cx", "PROPERTY"),
        ("ix_a", "PROPERTY"),
        ("rg_b", "PROPERTY"),
        ("rg_b", "RANGE"),
    ]
    g.cypher("DROP INDEX ix_a")
    assert not g.has_index("L", "a")
    assert [n for n, _ in _names(g)] == ["cx", "rg_b", "rg_b"]


@pytest.mark.parametrize("storage", MODES)
def test_a_relationship_index_name_survives_a_crash(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.cypher("MATCH (a:L {a: 1}), (b:L {a: 2}) CREATE (a)-[:R {k: 1}]->(b)")
g.cypher("CREATE INDEX rix FOR ()-[r:R]-() ON (r.k)")
""",
    )
    g = _reopen(tmp_path)
    assert _names(g) == [("rix", "PROPERTY")]
    g.cypher("DROP INDEX rix")
    assert _names(g) == []


@pytest.mark.parametrize("storage", MODES)
def test_a_drop_by_name_after_the_last_checkpoint_removes_the_name(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
g.cypher("CREATE INDEX ix_b FOR (n:L) ON (n.b)")
g.save(path)
g.cypher("DROP INDEX ix_a")
""",
    )
    g = _reopen(tmp_path)
    assert _names(g) == [("ix_b", "PROPERTY")]
    # The name went with the index: a later index on the property is unnamed.
    g.cypher("CREATE INDEX FOR (n:L) ON (n.a)")
    assert _names(g) == [("L.a", "PROPERTY"), ("ix_b", "PROPERTY")]


@pytest.mark.parametrize("storage", MODES)
def test_a_drop_by_canonical_name_releases_the_registered_name(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.cypher("CREATE INDEX ix_a FOR (n:L) ON (n.a)")
g.save(path)
g.cypher("DROP INDEX `L.a`")
g.cypher("CREATE INDEX FOR (n:L) ON (n.a)")
""",
    )
    g = _reopen(tmp_path)
    assert _names(g) == [("L.a", "PROPERTY")]


@pytest.mark.parametrize("storage", MODES)
def test_a_rename_after_a_checkpoint_replays_over_the_checkpointed_name(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.cypher("CREATE INDEX first FOR (n:L) ON (n.a)")
g.save(path)
g.cypher("DROP INDEX first")
g.cypher("CREATE INDEX second FOR (n:L) ON (n.a)")
""",
    )
    g = _reopen(tmp_path)
    assert _names(g) == [("second", "PROPERTY")]


@pytest.mark.parametrize("storage", MODES)
def test_a_named_constraint_and_its_owned_index_survive_a_crash(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.cypher("CREATE CONSTRAINT uq_a FOR (n:L) REQUIRE n.a IS UNIQUE")
""",
    )
    g = _reopen(tmp_path)
    rows = g.cypher("SHOW INDEXES YIELD name, owningConstraint").to_list()
    assert [(r["name"], r["owningConstraint"]) for r in rows] == [("uq_a", "uq_a")]
    g.cypher("DROP CONSTRAINT uq_a")
    assert _names(g) == []


@pytest.mark.parametrize("storage", MODES)
def test_a_named_text_index_survives_a_crash_and_drops_by_name(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.build_text_index("L", "text", name="tx")
""",
    )
    g = _reopen(tmp_path)
    assert g.has_text_index("L", "text")
    assert _names(g) == [("tx", "FULLTEXT")]
    g.cypher("DROP INDEX tx")
    assert not g.has_text_index("L", "text")


@pytest.mark.parametrize("storage", MODES)
def test_a_dropped_text_index_loses_its_name_across_a_crash(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.build_text_index("L", "text", name="tx")
g.save(path)
assert g.drop_text_index("L", "text")
g.build_text_index("L", "text")
""",
    )
    g = _reopen(tmp_path)
    assert g.has_text_index("L", "text")
    assert _names(g) == [("L.text", "FULLTEXT")]


@pytest.mark.parametrize("storage", MODES)
def test_a_named_vector_index_survives_a_crash_and_drops_by_name(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.set_embeddings("L", "text", {1: [0.6, 0.8], 2: [0.8, 0.6]})
g.build_vector_index("L", "text", name="vx")
""",
    )
    g = _reopen(tmp_path)
    assert g.has_vector_index("L", "text")
    assert _names(g) == [("vx", "VECTOR")]
    g.cypher("DROP INDEX vx")
    assert not g.has_vector_index("L", "text")


@pytest.mark.parametrize("storage", MODES)
def test_a_dropped_vector_index_loses_its_name_across_a_crash(tmp_path, storage):
    _child(
        tmp_path,
        storage,
        SEED
        + """
g.set_embeddings("L", "text", {1: [0.6, 0.8], 2: [0.8, 0.6]})
g.build_vector_index("L", "text", name="vx")
g.drop_vector_index("L", "text")
""",
    )
    g = _reopen(tmp_path)
    assert not g.has_vector_index("L", "text")
    assert _names(g) == []
