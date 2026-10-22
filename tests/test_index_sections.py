"""Type, id and peer-count indexes recorded in a v9 `.kgl` file.

A save of a graph with 65,536 nodes or 262,144 relationships or more records
the indexes a load would otherwise rebuild; a load into `mapped` storage serves
them from the file and a load into `memory` ignores them. The Rust suite covers
the engine paths (section round trips, writes, rollback, damaged sections);
these tests pin what the Python surface promises.
"""

from __future__ import annotations

import json
import struct
import sys

import numpy as np
import pandas as pd
import pytest

import kglite

NODES = 70_000
EDGES = 270_000
V9 = b"RGF\x09"
V8 = b"RGF\x08"

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="Auto never maps on Windows")

PROBES = [
    "MATCH (d:Doc {id: 1234}) RETURN d.n",
    "MATCH (d:Doc {id: 69999}) RETURN d.n",
    "MATCH (d:Doc {id: 70001}) RETURN d.n",
    "MATCH (d:Doc) RETURN count(d), sum(d.n)",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN b.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 5",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN a.id AS i, count(b) AS c ORDER BY c DESC, i LIMIT 5",
    "MATCH (a:Doc {id: 77})-[:LINKS]->(b) RETURN count(b)",
]


def _build() -> kglite.KnowledgeGraph:
    rng = np.random.default_rng(3)
    graph = kglite.KnowledgeGraph()
    nodes = pd.DataFrame(
        {
            "id": np.arange(NODES),
            "title": [f"d{i}" for i in range(NODES)],
            "n": rng.integers(0, 100, NODES),
        }
    )
    graph.add_nodes(nodes, "Doc", "id", "title")
    edges = pd.DataFrame({"s": rng.integers(0, NODES, EDGES), "t": rng.integers(0, NODES, EDGES)})
    graph.add_relationships(edges, "LINKS", "Doc", "s", "Doc", "t", distinct=True)
    return graph


@pytest.fixture(scope="module")
def saved(tmp_path_factory):
    path = str(tmp_path_factory.mktemp("idx") / "g.kgl")
    _build().save(path)
    return path


def _metadata(path: str) -> dict:
    with open(path, "rb") as f:
        head = f.read(13)
        length = struct.unpack("<I", head[9:13])[0]
        return json.loads(f.read(length - 4))


def _section_names(path: str) -> list[str]:
    return [entry["name"] for entry in _metadata(path).get("mappable", [])]


def _answers(graph: kglite.KnowledgeGraph) -> list:
    return [graph.cypher(q).to_list() for q in PROBES]


def test_a_large_graph_records_its_indexes_and_compact_does_not(saved, tmp_path):
    with open(saved, "rb") as f:
        assert f.read(4) == V9
    names = _section_names(saved)
    for expected in [
        "idx:type:Doc",
        "idx:id:Doc:keys",
        "idx:id:Doc:nodes",
        "idx:peer:LINKS:out",
        "idx:peer:LINKS:in",
    ]:
        assert expected in names, names

    compact = str(tmp_path / "compact.kgl")
    _build().save(compact, layout="compact")
    with open(compact, "rb") as f:
        assert f.read(4) == V8
    assert not _section_names(compact)


def test_every_storage_mode_answers_identically(saved):
    memory = kglite.load(saved, storage="memory")
    mapped = kglite.load(saved, storage="mapped")
    assert _answers(mapped) == _answers(memory)
    assert mapped.cypher(PROBES[2]).to_list() == []


def test_writes_after_a_mapped_load_match_memory(saved):
    memory = kglite.load(saved, storage="memory")
    mapped = kglite.load(saved, storage="mapped")
    steps = [
        "CREATE (:Doc {id: 70001, n: 7})",
        "MATCH (d:Doc {id: 1234}) SET d.n = 999",
        "MATCH (a:Doc {id: 77}), (b:Doc {id: 78}) CREATE (a)-[:LINKS]->(b)",
        "MATCH (d:Doc {id: 69999}) DETACH DELETE d",
    ]
    for step in steps:
        memory.cypher(step)
        mapped.cypher(step)
        assert _answers(mapped) == _answers(memory), step
    assert mapped.cypher(PROBES[2]).to_list() == [{"d.n": 7}]


def test_a_failed_statement_leaves_a_mapped_graph_unchanged(saved):
    mapped = kglite.load(saved, storage="mapped")
    before = _answers(mapped)
    with pytest.raises(Exception):
        mapped.cypher("CREATE (:Doc {id: 80001, n: 1}), (:Doc {id: 80002, bad: duration({months: 2147483648})})")
    assert _answers(mapped) == before
    assert mapped.cypher("MATCH (d:Doc {id: 80001}) RETURN d.n").to_list() == []


def test_a_resave_after_writes_reloads_correctly(saved, tmp_path):
    mapped = kglite.load(saved, storage="mapped")
    mapped.cypher("CREATE (:Doc {id: 70001, n: 7})")
    mapped.cypher("MATCH (a:Doc {id: 77}), (b:Doc {id: 78}) CREATE (a)-[:LINKS]->(b)")
    again = str(tmp_path / "again.kgl")
    mapped.save(again)
    reloaded = kglite.load(again, storage="mapped")
    assert _answers(reloaded) == _answers(mapped)
    assert reloaded.cypher(PROBES[2]).to_list() == [{"d.n": 7}]


def _data_region_base(meta: dict, path: str) -> int:
    with open(path, "rb") as f:
        length = struct.unpack("<I", f.read(13)[9:13])[0]
    compressed = sum(v for k, v in meta.items() if k.endswith("_compressed_size"))
    compressed += sum(c["compressed_size"] for c in meta.get("column_sections", []))
    end = 13 + length + compressed
    return (end + 63) // 64 * 64


def test_a_damaged_index_section_refuses_a_mapped_load(saved, tmp_path):
    damaged = str(tmp_path / "damaged.kgl")
    with open(saved, "rb") as f:
        raw = bytearray(f.read())
    meta = _metadata(saved)
    entry = next(e for e in meta["mappable"] if e["name"] == "idx:type:Doc")
    raw[_data_region_base(meta, saved) + entry["offset"] + entry["len"] // 2] ^= 0x40
    with open(damaged, "wb") as f:
        f.write(raw)

    with pytest.raises(Exception, match="corrupt"):
        kglite.load(damaged, storage="mapped")
    # Memory storage never reads the recorded indexes, so it loads and answers.
    assert kglite.load(damaged, storage="memory").cypher(PROBES[3]).to_list() == (
        kglite.load(saved, storage="memory").cypher(PROBES[3]).to_list()
    )


STRING_PROBES = [
    "MATCH (p:Page {id: 'page-1234'}) RETURN p.n",
    "MATCH (p:Page {id: 'page-12345'}) RETURN p.n",
    "MATCH (p:Page {id: 'page-123456'}) RETURN p.n",
    "MATCH (p:Page {id: 'page-'}) RETURN p.n",
    "MATCH (p:Page {id: 'sider-åø'}) RETURN p.n",
    "MATCH (p:Page {id: '日本語'}) RETURN p.n",
    "MATCH (p:Page {id: ''}) RETURN p.n",
    "MATCH (p:Page {id: 1234}) RETURN p.n",
    "MATCH (p:Page) RETURN count(p), sum(p.n)",
]


def _build_strings() -> kglite.KnowledgeGraph:
    ids = [f"page-{i}" for i in range(NODES)] + ["", "page-", "sider-åø", "日本語"]
    graph = kglite.KnowledgeGraph()
    frame = pd.DataFrame({"id": ids, "title": ids, "n": np.arange(len(ids))})
    graph.add_nodes(frame, "Page", "id", "title")
    return graph


@pytest.fixture(scope="module")
def saved_strings(tmp_path_factory):
    path = str(tmp_path_factory.mktemp("idxs") / "s.kgl")
    _build_strings().save(path)
    return path


def _string_answers(graph: kglite.KnowledgeGraph) -> list:
    return [graph.cypher(q).to_list() for q in STRING_PROBES]


def test_string_ids_are_recorded_and_served_like_memory(saved_strings):
    names = _section_names(saved_strings)
    assert "idx:id:Page:keys" in names and "idx:id:Page:nodes" in names, names
    memory = kglite.load(saved_strings, storage="memory")
    mapped = kglite.load(saved_strings, storage="mapped")
    assert _string_answers(mapped) == _string_answers(memory)
    assert mapped.cypher(STRING_PROBES[0]).to_list() == [{"p.n": 1234}]
    assert mapped.cypher(STRING_PROBES[2]).to_list() == []
    assert mapped.cypher(STRING_PROBES[7]).to_list() == []


def test_writes_to_string_ids_after_a_mapped_load_match_memory(saved_strings, tmp_path):
    memory = kglite.load(saved_strings, storage="memory")
    mapped = kglite.load(saved_strings, storage="mapped")
    steps = [
        "CREATE (:Page {id: 'page-new', n: -1})",
        "MATCH (p:Page {id: 'page-1234'}) SET p.n = 999",
        "MATCH (p:Page {id: 'page-12345'}) DETACH DELETE p",
        "CREATE (:Page {id: 'page-12345', n: 5})",
    ]
    for step in steps:
        memory.cypher(step)
        mapped.cypher(step)
        assert _string_answers(mapped) == _string_answers(memory), step
        for probe in ["page-new", "page-12345", "page-1234"]:
            q = f"MATCH (p:Page {{id: '{probe}'}}) RETURN p.n"
            assert mapped.cypher(q).to_list() == memory.cypher(q).to_list(), (step, probe)
    again = str(tmp_path / "again.kgl")
    mapped.save(again)
    reloaded = kglite.load(again, storage="mapped")
    assert _string_answers(reloaded) == _string_answers(memory)
