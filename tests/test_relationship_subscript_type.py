"""``r['type']`` reads the relationship type that ``keys(r)`` reports.

Red proof: before the fix ``keys(r)`` listed ``type`` while ``r['type']`` was
null, so ``[k IN keys(r) | r[k]]`` carried a null for it.
"""

from __future__ import annotations

import pytest

import kglite

MODES = pytest.mark.parametrize("mode", ["memory", "mapped", "disk"])


def _graph(mode, tmp_path, setup) -> kglite.KnowledgeGraph:
    if mode == "memory":
        g = kglite.KnowledgeGraph()
    elif mode == "mapped":
        g = kglite.KnowledgeGraph(storage="mapped")
    else:
        g = kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    g.cypher(setup).to_list()
    return g


@MODES
def test_relationship_subscript_reads_the_type_key_keys_reports(mode, tmp_path) -> None:
    g = _graph(mode, tmp_path, "CREATE (:A)-[:T {w: 1}]->(:B)")
    rows = g.cypher("MATCH ()-[r]->() RETURN keys(r) AS k, r['type'] AS t, [key IN keys(r) | r[key]] AS v").to_list()
    assert rows == [{"k": ["type", "w"], "t": "T", "v": ["T", 1]}]
