"""Id lookups on a durable session stay correct while a write stream publishes versions.

A reader's `MATCH (n:Leaf {id: $x})` builds a per-type id index lazily, and every
published version is cut from the writer, so the index is shared across versions
while the type is unchanged (core: `id_index_shared`). The timing benefit is
measured in release; these tests pin that the answers are right after the
writer adds, deletes and recreates nodes of the queried type, and that an
unrelated write stream leaves them alone.
"""

from __future__ import annotations

import kglite

LEAVES = 60
BY_ID = "MATCH (n:Leaf {id: $x}) RETURN n.k AS k"
BY_IDS = "MATCH (n:Leaf) WHERE n.id IN $xs RETURN n.k AS k ORDER BY k"


def _seed(s) -> None:
    for i in range(LEAVES):
        s.execute("CREATE (:Leaf {id: $i, k: $i})", params={"i": i})


def _one(s, x: int):
    rows = s.cypher(BY_ID, params={"x": x}).to_list()
    return [r["k"] for r in rows]


def test_reader_lookups_survive_an_unrelated_write_stream(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="normal") as s:
        _seed(s)
        assert _one(s, 7) == [7]
        for i in range(40):
            s.execute("CREATE (:Product {id: $i, k: $i})", params={"i": i})
            assert _one(s, 7) == [7]
            assert _one(s, LEAVES + 5) == []
        assert [r["k"] for r in s.cypher(BY_IDS, params={"xs": [1, 2, 999]}).to_list()] == [1, 2]


def test_reader_lookups_follow_writes_to_the_queried_type(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="normal") as s:
        _seed(s)
        assert _one(s, 7) == [7]

        s.execute("CREATE (:Leaf {id: 1000, k: 1000})")
        assert _one(s, 1000) == [1000]
        assert _one(s, 7) == [7]

        s.execute("MATCH (n:Leaf {k: 7}) DETACH DELETE n")
        assert _one(s, 7) == []
        assert _one(s, 8) == [8]

        s.execute("CREATE (:Leaf {id: 7, k: 70})")
        assert _one(s, 7) == [70]
        assert [r["k"] for r in s.cypher(BY_IDS, params={"xs": [7, 8, 1000]}).to_list()] == [8, 70, 1000]


def test_a_held_snapshot_keeps_its_own_ids(tmp_path):
    with kglite.open_session(str(tmp_path / "app.kgl"), durable="normal") as s:
        _seed(s)
        assert _one(s, 7) == [7]
        view = s.snapshot()
        s.execute("CREATE (:Leaf {id: 1000, k: 1000})")
        s.execute("MATCH (n:Leaf {k: 7}) DETACH DELETE n")
        assert _one(s, 1000) == [1000]
        assert _one(s, 7) == []
        assert view.cypher(BY_ID, params={"x": 1000}).to_list() == []
        assert [r["k"] for r in view.cypher(BY_ID, params={"x": 7}).to_list()] == [7]
