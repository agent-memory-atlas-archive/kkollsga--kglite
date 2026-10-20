"""Embedding stores served from a v9 `.kgl` file: placement, parity with the heap,
promotion on write, integrity, and the memory they leave resident.

A store of 4 MiB or more saves as v9 (vectors and norms stored raw, 64-byte
aligned); a load maps it according to `embeddings=`. The Rust suite covers the
engine paths (clones, undo, HNSW, edge stores, the Windows policy); these tests
pin what the Python surface promises.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap

import pytest

import kglite

np = pytest.importorskip("numpy")

DIM = 1024
SMALL = 1100  # 1100 x 1024 x 4 = 4.4 MiB, just over the writer's floor
BIG = 17_000  # 69.6 MB, over the 64 MB Auto floor
V9 = b"RGF\x09"
V8 = b"RGF\x08"


def _vectors(n: int, seed: int = 1):
    return np.random.default_rng(seed).standard_normal((n, DIM), dtype=np.float32)


def _graph(n: int = SMALL) -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    graph.cypher(f"UNWIND range(1, {n}) AS i CREATE (:Doc {{id: i, title: 'd', text: 't'}})")
    vectors = _vectors(n)
    graph.set_embeddings("Doc", "text", {i + 1: vectors[i].tolist() for i in range(n)})
    return graph


@pytest.fixture(scope="module")
def saved(tmp_path_factory):
    path = str(tmp_path_factory.mktemp("mapped") / "g.kgl")
    _graph().save(path)
    return path


def _magic(path: str) -> bytes:
    with open(path, "rb") as handle:
        return handle.read(4)


def _storage(graph: kglite.KnowledgeGraph) -> str:
    (row,) = graph.list_embeddings()
    return row["storage"]


def _ranking(graph: kglite.KnowledgeGraph, query, **kwargs):
    hits = graph.select("Doc").vector_search("text", list(query), top_k=7, **kwargs)
    return [(hit["id"], hit["score"]) for hit in hits]


def test_a_large_store_saves_as_v9_and_compact_writes_v8(tmp_path):
    graph = _graph()
    auto = str(tmp_path / "auto.kgl")
    compact = str(tmp_path / "compact.kgl")
    graph.save(auto)
    graph.save(compact, layout="compact")
    assert _magic(auto) == V9
    assert _magic(compact) == V8
    assert _storage(kglite.load(compact, embeddings="mapped")) == "heap"  # v8: nothing to map


def test_a_small_store_keeps_the_v8_file(tmp_path):
    graph = kglite.KnowledgeGraph()
    graph.cypher("CREATE (:Doc {id: 1, title: 'a', text: 't'})")
    graph.set_embeddings("Doc", "text", {1: [1.0, 0.0]})
    path = str(tmp_path / "small.kgl")
    graph.save(path)
    assert _magic(path) == V8


def test_every_policy_gives_the_same_search_results(saved):
    query = _vectors(1, seed=9)[0]
    mapped = kglite.load(saved, embeddings="mapped")
    memory = kglite.load(saved, embeddings="memory")
    auto = kglite.load(saved)
    assert (_storage(mapped), _storage(memory), _storage(auto)) == ("mapped", "heap", "heap")
    expected = _ranking(memory, query)
    assert _ranking(mapped, query) == expected
    assert _ranking(auto, query) == expected
    assert _ranking(mapped, query, exact=True) == expected
    assert mapped.embedding_diagnostics("Doc")[0]["storage"] == "mapped"


def test_embedding_arguments_are_validated(saved):
    with pytest.raises(kglite.ArgumentError, match="embeddings must be"):
        kglite.load(saved, embeddings="always")
    with pytest.raises(kglite.ArgumentError, match="verify must be"):
        kglite.load(saved, verify="sometimes")
    with pytest.raises(ValueError, match="layout must be"):
        _graph(10).save(saved + ".x", layout="tiny")


def test_the_first_write_promotes_only_the_written_graph(saved):
    writer = kglite.load(saved, embeddings="mapped")
    reader = kglite.load(saved, embeddings="mapped")
    copy = writer.copy()
    assert _storage(copy) == "mapped"  # a copy shares the mapping

    marker = [1.0] + [0.0] * (DIM - 1)
    writer.set_embeddings("Doc", "text", {1: marker})  # replaces the whole store
    assert _storage(writer) == "heap"
    assert _storage(reader) == "mapped"
    assert _storage(copy) == "mapped"
    assert len(writer.list_embeddings()) == 1
    # The untouched graphs still answer from the mapping.
    assert _ranking(reader, _vectors(1, seed=9)[0])
    assert _ranking(copy, _vectors(1, seed=9)[0]) == _ranking(reader, _vectors(1, seed=9)[0])


def test_a_cypher_delete_promotes_the_store_and_search_follows(saved):
    graph = kglite.load(saved, embeddings="mapped")
    victim = graph.select("Doc").vector_search("text", _vectors(1, seed=9)[0].tolist(), top_k=1)[0]["id"]
    graph.cypher(f"MATCH (n:Doc {{id: {victim}}}) DETACH DELETE n")
    assert _storage(graph) == "heap"
    assert victim not in [
        h["id"] for h in graph.select("Doc").vector_search("text", _vectors(1, seed=9)[0].tolist(), top_k=20)
    ]


def test_save_over_the_loaded_path_keeps_the_graph_usable(tmp_path):
    path = str(tmp_path / "g.kgl")
    _graph().save(path)
    graph = kglite.load(path, embeddings="mapped")
    before = _ranking(graph, _vectors(1, seed=9)[0])
    graph.save()  # rename over the file this graph is mapped from
    assert _magic(path) == V9
    assert _storage(graph) == "mapped"
    assert _ranking(graph, _vectors(1, seed=9)[0]) == before
    assert _ranking(kglite.load(path, embeddings="mapped"), _vectors(1, seed=9)[0]) == before


def test_verify_integrity_reports_intact_and_corrupt_sections(tmp_path):
    path = str(tmp_path / "g.kgl")
    _graph().save(path)
    graph = kglite.load(path, embeddings="mapped")
    report = graph.verify_integrity()
    assert report["ok"] is True
    assert [s["name"] for s in report["sections"]] == ["embeddings:Doc.text_emb"]
    assert report["sections"][0]["mapped"] is True and report["sections"][0]["error"] is None
    assert kglite.load(path, embeddings="memory").verify_integrity() == {"ok": True, "sections": []}

    # The norms are the last section in the file: flip a byte inside them.
    size = os.path.getsize(path)
    with open(path, "r+b") as handle:
        handle.seek(size - 40)
        byte = handle.read(1)
        handle.seek(size - 40)
        handle.write(bytes([byte[0] ^ 0x55]))

    corrupt = kglite.load(path, embeddings="mapped")  # lazy: the load itself succeeds
    assert corrupt.verify_integrity()["ok"] is False
    with pytest.raises(Exception, match="corrupt"):
        _ranking(corrupt, _vectors(1, seed=9)[0])
    with pytest.raises(Exception, match="corrupt"):
        kglite.load(path, embeddings="mapped", verify="eager")


def _probe(path: str, policy: str, env: dict[str, str] | None = None) -> dict[str, float]:
    """Load `path` in a fresh process and report footprint growth and storage."""
    script = textwrap.dedent(
        """
        import ctypes, ctypes.util, json, os, struct, sys
        import kglite

        def footprint():
            libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
            buf = ctypes.create_string_buffer(512)
            libc.proc_pid_rusage(ctypes.c_int(os.getpid()), ctypes.c_int(0), ctypes.byref(buf))
            return struct.unpack_from("=8Q", buf.raw, 16)[7]

        kglite.trim_memory()
        base = footprint()
        g = kglite.load(sys.argv[1], embeddings=sys.argv[2])
        kglite.trim_memory()
        grown = footprint() - base
        print(json.dumps({"grown": grown, "storage": g.list_embeddings()[0]["storage"]}))
        """
    )
    result = subprocess.run(
        [sys.executable, "-c", script, path, policy],
        capture_output=True,
        text=True,
        check=True,
        env={**os.environ, **(env or {})},
    )
    import json

    return json.loads(result.stdout.strip().splitlines()[-1])


@pytest.fixture(scope="module")
def big(tmp_path_factory):
    path = str(tmp_path_factory.mktemp("big") / "big.kgl")
    graph = kglite.KnowledgeGraph()
    graph.cypher(f"UNWIND range(1, {BIG}) AS i CREATE (:Doc {{id: i, title: 'd', text: 't'}})")
    row = [0.5] * DIM
    graph.set_embeddings("Doc", "text", {i: row for i in range(1, BIG + 1)})
    graph.save(path)
    return path


def test_auto_maps_a_store_over_the_floor_and_the_kill_switch_overrides(big):
    store_bytes = BIG * DIM * 4
    assert os.path.getsize(big) >= store_bytes  # stored raw
    assert _storage(kglite.load(big)) == "mapped"  # Auto, 69.6 MB >= 64 MB
    assert _storage(kglite.load(big, embeddings="memory")) == "heap"
    # The environment switch outranks even an explicit "mapped" (fresh process).
    probe = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys, kglite; g = kglite.load(sys.argv[1], embeddings='mapped');"
            "print(g.list_embeddings()[0]['storage'])",
            big,
        ],
        capture_output=True,
        text=True,
        check=True,
        env={**os.environ, "KGLITE_MMAP": "0"},
    )
    assert probe.stdout.strip() == "heap"


@pytest.mark.skipif(
    sys.platform != "darwin",
    reason="phys_footprint excludes clean file pages on macOS only; Linux RSS counts touched pages",
)
def test_a_mapped_load_leaves_the_store_out_of_the_footprint(big):
    store_bytes = BIG * DIM * 4
    mapped = _probe(big, "mapped")
    heap = _probe(big, "memory")
    assert mapped["storage"] == "mapped" and heap["storage"] == "heap"
    assert heap["grown"] >= store_bytes * 0.9, heap
    assert mapped["grown"] <= store_bytes * 0.25, (mapped, heap)
