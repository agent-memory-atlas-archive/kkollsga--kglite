"""The compact int64 id index across reopen -> append -> save -> reopen cycles.

A register keyed by ~3.1e12 integer ids persists its id index as a sorted
``(i64, u32)`` array searched in the mapping (12 bytes an id), and the first write
after a reopen layers a delta over it. These goldens run the register-shaped
fixture through two such cycles, with deletions and an overwrite in the second,
and compare every answer to a pandas oracle after each reopen: an id resolved
from the mapping, from the delta, an id that was deleted, the title index, edge
endpoints resolved through the index, and a valid-time count.

Run: pytest tests/test_disk_id_index_cycles.py
"""

from __future__ import annotations

from pathlib import Path
import struct
import warnings

import pandas as pd
import pytest

import kglite
from tests.fixtures.disk_generation import current_generation
from tests.fixtures.register_scale import ANCHOR_TYPE, REL, TYPE, chunk

CHUNK = 20_000
VARIANT_INT64 = 2


def _add(graph, part) -> None:
    graph.add_nodes(part.versions, TYPE, "id", "ident")
    graph.add_nodes(part.anchors, ANCHOR_TYPE, "id", conflict_handling="skip")
    graph.add_relationships(part.edges, REL, TYPE, "id", ANCHOR_TYPE, "ident")


def _directory(path: Path) -> list[tuple[int, int, int]]:
    """``(variant, num_entries, payload_len)`` of the current generation's id index."""
    raw = (current_generation(path) / "id_indices.bin").read_bytes()
    assert raw[:8] == b"KGLIIDXR" and struct.unpack("<I", raw[8:12])[0] == 3
    (count,) = struct.unpack("<I", raw[12:16])
    entries = []
    for i in range(count):
        at = 32 + 48 * i
        (entries_n,) = struct.unpack("<Q", raw[at + 16 : at + 24])
        (payload_len,) = struct.unpack("<Q", raw[at + 32 : at + 40])
        entries.append((raw[at + 8], entries_n, payload_len))
    return entries


def _assert_sorted_int64_entry(path: Path, live_versions: int) -> None:
    ours = [entry for entry in _directory(path) if entry[0] == VARIANT_INT64 and entry[1] == live_versions]
    assert ours, f"no Int64Sorted entry of {live_versions} ids in {_directory(path)}"
    assert ours[0][2] == 12 * live_versions


def _check(graph, frame: pd.DataFrame, deleted: list[int]) -> None:
    live = frame[~frame["id"].isin(deleted)]
    assert graph.cypher(f"FOR VALID_TIME ALL MATCH (p:{TYPE}) RETURN count(*) AS c").to_list() == [{"c": len(live)}]
    assert graph.cypher(
        f"FOR VALID_TIME ALL MATCH (:{TYPE})-[:{REL}]->(:{ANCHOR_TYPE}) RETURN count(*) AS c"
    ).to_list() == [{"c": len(live)}]
    for position in (0, CHUNK - 1, CHUNK, len(frame) // 2, len(frame) - 1):
        row = frame.iloc[position]
        got = graph.cypher(
            f"FOR VALID_TIME ALL MATCH (p:{TYPE} {{id: $i}}) RETURN p.title AS t, p.status AS s",
            params={"i": int(row.id)},
        ).to_list()
        if int(row.id) in deleted:
            assert got == []
        else:
            assert got == [{"t": int(row.ident), "s": row.status}], (position, got)
    for gone in deleted:
        assert graph.cypher(
            f"FOR VALID_TIME ALL MATCH (p:{TYPE} {{id: $i}}) RETURN count(*) AS c", params={"i": gone}
        ).to_list() == [{"c": 0}]
    # A float spelling of a stored integer id names the same version.
    row = live.iloc[len(live) // 3]
    assert graph.cypher(
        f"FOR VALID_TIME ALL MATCH (p:{TYPE}) WHERE p.id = $i RETURN p.title AS t", params={"i": float(row.id)}
    ).to_list() == [{"t": int(row.ident)}]
    missing = int(frame["id"].max()) + 1
    assert graph.cypher(
        f"FOR VALID_TIME ALL MATCH (p:{TYPE} {{id: $i}}) RETURN count(*) AS c", params={"i": missing}
    ).to_list() == [{"c": 0}]
    at = pd.Timestamp("2005-06-30T12:34:56.789012")
    expected = int(((live["valid_from"] <= at) & (live["valid_to"].isna() | (live["valid_to"] > at))).sum())
    assert 0 < expected < len(live)
    query = f"FOR VALID_TIME AS OF datetime('{at.isoformat()}') MATCH (p:{TYPE}) RETURN count(*) AS c"
    assert graph.cypher(query).to_list() == [{"c": expected}]


def test_two_reopen_append_save_reopen_cycles_keep_every_answer(tmp_path):
    path = tmp_path / "employment"
    chunks = [chunk(i, CHUNK) for i in range(4)]
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=str(path))
        _add(graph, chunks[0])
        graph.set_temporal(TYPE, "valid_from", "valid_to", convention="half_open")
        _add(graph, chunks[1])
        graph.save()
        del graph
        _assert_sorted_int64_entry(path, 2 * CHUNK)
        frame = pd.concat([c.versions for c in chunks[:2]], ignore_index=True)
        _check(kglite.load(str(path)), frame, [])

        # Cycle one: reopen, append a chunk, save, reopen.
        graph = kglite.load(str(path))
        _add(graph, chunks[2])
        graph.save()
        del graph
        frame = pd.concat([c.versions for c in chunks[:3]], ignore_index=True)
        _assert_sorted_int64_entry(path, 3 * CHUNK)
        _check(kglite.load(str(path)), frame, [])

        # Cycle two: reopen, append, overwrite existing ids, delete some, save, reopen.
        graph = kglite.load(str(path))
        _add(graph, chunks[3])
        touched = frame.iloc[:5].copy()
        touched["status"] = "terminated"
        graph.add_nodes(touched, TYPE, "id", "ident", conflict_handling="update")
        frame.loc[frame.index[:5], "status"] = "terminated"
        frame = pd.concat([frame, chunks[3].versions], ignore_index=True)
        deleted = [int(frame["id"].iloc[i]) for i in (7, CHUNK + 11, 2 * CHUNK + 13)]
        for gone in deleted:
            graph.cypher(f"MATCH (p:{TYPE} {{id: $i}}) DETACH DELETE p", params={"i": gone})
        graph.save()
        del graph
        _assert_sorted_int64_entry(path, 4 * CHUNK - len(deleted))
        reopened = kglite.load(str(path))
        _check(reopened, frame, deleted)
        row = frame.iloc[2]
        assert reopened.cypher(
            f"MATCH (p:{TYPE} {{id: $i}}) RETURN p.status AS s", params={"i": int(row.id)}
        ).to_list() == [{"s": "terminated"}]


def test_a_version_2_id_index_still_loads_and_is_rewritten_as_version_3(tmp_path):
    """The fixture 0.19.0 wrote (Employment ids past u32, a `General` map at version 2)
    opens; its next save writes the sorted layout."""
    fixtures = Path(__file__).parent / "fixtures" / "kgl_v6" / "disk"
    import shutil

    directory = tmp_path / "disk"
    shutil.copytree(fixtures, directory)
    before = (current_generation(directory) / "id_indices.bin").read_bytes()
    assert struct.unpack("<I", before[8:12])[0] == 2, "the fixture is a 0.19.0 (version 2) index"
    graph = kglite.load(str(directory))
    count = graph.cypher("MATCH (n) RETURN count(n) AS c").to_list()
    graph.save()
    del graph
    after = (current_generation(directory) / "id_indices.bin").read_bytes()
    assert struct.unpack("<I", after[8:12])[0] == 3
    assert kglite.load(str(directory)).cypher("MATCH (n) RETURN count(n) AS c").to_list() == count


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__]))
