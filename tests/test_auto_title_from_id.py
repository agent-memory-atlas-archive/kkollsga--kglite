"""A node created without a title is titled `<Label>_<id>`.

The title used to come from the storage slot count, which every backend reuses
differently after a delete: titles repeated within one graph and differed
between memory, mapped and disk. Built from the id, the title is unique
wherever the id is, and identical in every storage mode.
"""

from __future__ import annotations

import pytest

import kglite


def _graph(storage, tmp_path):
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "g"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def _titles(storage, tmp_path):
    graph = _graph(storage, tmp_path)
    for query in [
        "UNWIND range(1, 3) AS i CREATE (:L {v: i})",
        "MATCH (n:L {v: 3}) DELETE n",
        "CREATE (:L {v: 4})",
        "MATCH (n:L {v: 1}) DELETE n",
        "CREATE (:L {v: 5})",
        "CREATE (:L {v: 6})",
        "MERGE (:L {v: 7})",
        "CREATE (:L {v: 8, id: 's1'})",
    ]:
        graph.cypher(query)
    return graph.cypher("MATCH (n:L) RETURN n.v AS v, n.id AS id, n.title AS title ORDER BY v").to_list()


@pytest.mark.parametrize("storage", ["default", "mapped", "disk"])
def test_auto_titles_are_built_from_the_id(storage, tmp_path) -> None:
    rows = _titles(storage, tmp_path)
    assert [row["title"] for row in rows] == [f"L_{row['id']}" for row in rows]
    assert len({row["title"] for row in rows}) == len(rows)
    assert rows[-1]["title"] == "L_s1"


def test_auto_titles_are_identical_in_every_storage_mode(tmp_path) -> None:
    golden = _titles("default", tmp_path / "a")
    assert [row["title"] for row in golden] == ["L_1", "L_3", "L_4", "L_5", "L_6", "L_s1"]
    assert _titles("mapped", tmp_path / "b") == golden
    assert _titles("disk", tmp_path / "c") == golden
