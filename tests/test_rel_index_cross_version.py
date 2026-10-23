"""A file with a relationship index or UNIQUE constraint opens in 0.21.0, and
a 0.21.0 file opens here.

The declaration lives under a new additive key of the ``.kgl`` metadata. An
older binary ignores the key, so it opens the file, reads every relationship and
simply has no index or enforcement; it drops the declaration when it saves
again. A newer binary reads an older file as one that declares neither.

Opt-in: point ``KGLITE_REFERENCE_PYTHON`` at an interpreter with the published
``kglite==0.21.0`` wheel installed in an isolated venv, e.g. ::

    uv venv /tmp/ref --python 3.12 && uv pip install --python /tmp/ref/bin/python kglite==0.21.0
    KGLITE_REFERENCE_PYTHON=/tmp/ref/bin/python pytest tests/test_rel_index_cross_version.py
"""

from __future__ import annotations

import json
import os
import subprocess

import pytest

import kglite

REFERENCE = os.environ.get("KGLITE_REFERENCE_PYTHON")
pytestmark = pytest.mark.skipif(not REFERENCE, reason="KGLITE_REFERENCE_PYTHON is not set")

READ_AND_RESAVE = """
import json, sys, kglite
g = kglite.load(sys.argv[1])
out = {
    "version": kglite.__version__,
    "file": kglite.__file__,
    "count": g.cypher("MATCH ()-[r:T]->() RETURN count(r) AS c").to_dicts()[0]["c"],
    "probe": g.cypher("MATCH ()-[r:T {k: 'a'}]->() RETURN count(r) AS c").to_dicts()[0]["c"],
    "indexes": g.cypher("SHOW INDEXES").to_dicts(),
}
g.cypher("MATCH (p:P {id: 2}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a'}]->(m)")  # no enforcement there
g.save(sys.argv[2])
print(json.dumps(out))
"""

WRITE_OLD = """
import sys, kglite
g = kglite.KnowledgeGraph()
g.cypher("CREATE (:P {id: 1}), (:P {id: 2}), (:M {id: 10})")
g.cypher("MATCH (p:P {id: 1}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a'}]->(m)")
g.save(sys.argv[1])
"""


def _reference(script: str, *args: str) -> str:
    done = subprocess.run(
        [REFERENCE, "-I", "-c", script, *args], capture_output=True, text=True, timeout=120, check=True
    )
    return done.stdout


def _new_file(path: str, declare: str) -> None:
    g = kglite.KnowledgeGraph()
    g.cypher("CREATE (:P {id: 1}), (:P {id: 2}), (:M {id: 10})")
    g.cypher("MATCH (p:P {id: 1}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a'}]->(m)")
    g.cypher(declare)
    g.save(path)


@pytest.mark.parametrize(
    "declare",
    [
        "CREATE INDEX FOR ()-[r:T]-() ON (r.k)",
        "CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    ],
)
def test_a_file_with_the_declaration_opens_in_the_reference_build(declare, tmp_path):
    path = str(tmp_path / "new.kgl")
    resaved = str(tmp_path / "resaved.kgl")
    _new_file(path, declare)
    report = json.loads(_reference(READ_AND_RESAVE, path, resaved).strip().splitlines()[-1])
    assert report["version"] == "0.21.0"
    assert not report["file"].startswith(os.path.dirname(os.path.dirname(kglite.__file__)))
    assert (report["count"], report["probe"]) == (1, 1), "every relationship reads, unindexed"
    assert report["indexes"] == [], "the reference build has no relationship index to list"
    # Saved again by the older binary the declaration is gone, and this build
    # reads the file as one that never had it.
    again = kglite.load(resaved)
    assert again.cypher("SHOW INDEXES").to_list() == []
    assert again.cypher("SHOW CONSTRAINTS").to_list() == []
    assert again.cypher("MATCH ()-[r:T {k: 'a'}]->() RETURN count(r) AS c").to_dicts() == [{"c": 2}]


def test_a_file_from_the_reference_build_opens_here_and_takes_a_declaration(tmp_path):
    path = str(tmp_path / "old.kgl")
    _reference(WRITE_OLD, path)
    g = kglite.load(path)
    assert g.cypher("SHOW INDEXES").to_list() == []
    g.cypher("CREATE INDEX FOR ()-[r:T]-() ON (r.k)")
    assert g.cypher("MATCH ()-[r:T {k: 'a'}]->() RETURN count(r) AS c").to_dicts() == [{"c": 1}]
    g.cypher("CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE")
    with pytest.raises(kglite.ConstraintViolationError):
        g.cypher("MATCH (p:P {id: 2}), (m:M {id: 10}) CREATE (p)-[:T {k: 'a'}]->(m)")
