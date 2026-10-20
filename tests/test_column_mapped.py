"""Property columns served from a v9 `.kgl` file: placement, parity with a
decoded load, promotion on write, spill files, integrity and footprint.

A column of 256 KB or more saves into the v9 data region (raw, 64-byte aligned);
a load maps it according to `columns=`. The Rust suite covers the engine paths
(promotion per column, snapshots, the Windows policy); these tests pin what the
Python surface promises.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap

import pytest

import kglite

ROWS = 40_000
V9 = b"RGF\x09"
V8 = b"RGF\x08"
PROBE = (
    "MATCH (n:Big) RETURN count(n) AS c, sum(n.score) AS s, sum(n.ratio) AS r, "
    "max(n.label) AS l, min(n.title) AS t, sum(CASE WHEN n.flag THEN 1 ELSE 0 END) AS f"
)

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="Auto never maps on Windows")


def _build() -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph()
    # `id` and `score` are scrambled so the delta form (kept only when 4x
    # smaller) does not win and every large column lands in the data region.
    graph.cypher(
        f"UNWIND range(1, {ROWS}) AS i CREATE (:Big {{id: (i * 7919) % 40009, seq: i, "
        "title: 't' + toString(i), score: (i * 7919) % 40009 * 104729, ratio: i / 7.0, "
        "label: 'row-' + toString(i) + '-ünï', flag: i % 2 = 0}) "
    )
    return graph


@pytest.fixture(scope="module")
def v9(tmp_path_factory):
    path = str(tmp_path_factory.mktemp("cols") / "g.kgl")
    _build().save(path)
    return path


def _col_sections(graph: kglite.KnowledgeGraph) -> list[str]:
    return [s["name"] for s in graph.verify_integrity()["sections"] if s["name"].startswith("col:")]


def _rows(graph):
    return graph.cypher(PROBE).to_list()


def test_a_default_save_is_v9_and_compact_is_v8(v9, tmp_path):
    with open(v9, "rb") as f:
        assert f.read(4) == V9
    compact = str(tmp_path / "compact.kgl")
    _build().save(compact, layout="compact")
    with open(compact, "rb") as f:
        assert f.read(4) == V8
    assert os.path.getsize(v9) > os.path.getsize(compact)  # raw columns are bigger


def test_every_placement_answers_identically(v9, tmp_path):
    expected = _rows(kglite.load(v9, columns="memory"))
    for policy in ("auto", "mapped", "memory"):
        assert _rows(kglite.load(v9, columns=policy)) == expected, policy
    for storage in ("memory", "mapped"):
        graph = kglite.load(v9, columns="mapped", storage=storage)
        assert _rows(graph) == expected, storage
        assert _col_sections(graph), storage
    assert _col_sections(kglite.load(v9)) != []  # Auto maps a 256 KB column
    assert _col_sections(kglite.load(v9, columns="memory")) == []
    compact = str(tmp_path / "compact.kgl")
    _build().save(compact, layout="compact")
    assert _rows(kglite.load(compact, columns="mapped")) == expected
    assert _col_sections(kglite.load(compact, columns="mapped")) == []  # v8 has nothing to map


def test_a_bad_columns_value_is_an_argument_error(v9):
    with pytest.raises(kglite.ArgumentError, match="columns must be"):
        kglite.load(v9, columns="always")


def test_open_session_takes_columns(v9):
    session = kglite.open_session(v9, columns="mapped")
    assert session.cypher(PROBE).to_list() == _rows(kglite.load(v9, columns="memory"))


def test_the_first_write_promotes_one_column_and_a_copy_keeps_reading_the_file(v9):
    graph = kglite.load(v9, columns="mapped")
    before = _col_sections(graph)
    snapshot = graph.copy()
    expected = snapshot.cypher("MATCH (n:Big) RETURN sum(n.ratio) AS r").to_list()
    graph.cypher("MATCH (n:Big) WHERE n.seq = 5 SET n.ratio = 0.5")
    assert graph.cypher("MATCH (n:Big) WHERE n.seq = 5 RETURN n.ratio AS r").to_list() == [{"r": 0.5}]
    after = _col_sections(graph)
    assert 0 < len(after) < len(before)
    assert snapshot.cypher("MATCH (n:Big) RETURN sum(n.ratio) AS r").to_list() == expected
    assert len(_col_sections(snapshot)) == len(before)
    graph.cypher("MATCH (n:Big) WHERE n.seq = 6 SET n.label = 'a much longer replacement label'")
    assert graph.cypher("MATCH (n:Big) WHERE n.seq = 6 RETURN n.label AS l").to_list() == [
        {"l": "a much longer replacement label"}
    ]


def test_saving_over_the_loaded_path_keeps_working(v9, tmp_path):
    path = str(tmp_path / "again.kgl")
    _build().save(path)
    graph = kglite.load(path, columns="mapped")
    expected = _rows(graph)
    graph.save(path)
    assert _rows(graph) == expected
    assert _rows(kglite.load(path, columns="mapped")) == expected
    assert graph.verify_integrity()["ok"]


def _flip(path: str, section_marker: bytes, offset: int) -> None:
    """Flip a byte inside a column section, found through the metadata table."""
    data = bytearray(open(path, "rb").read())
    meta_len = int.from_bytes(data[9:13], "little")
    meta = json.loads(bytes(data[13 : 13 + meta_len - 4]))
    entry = next(e for e in meta["mappable"] if e["name"] == section_marker.decode())
    sizes = [
        meta["topology_compressed_size"],
        sum(c["compressed_size"] for c in meta["column_sections"]),
    ] + [meta.get(k, 0) for k in meta if k.endswith("_compressed_size") and k not in ("topology_compressed_size",)]
    end = 13 + meta_len + sizes[0] + sizes[1] + sum(sizes[2:])
    base = (end + 63) // 64 * 64
    data[base + entry["offset"] + offset] ^= 0xFF
    open(path, "wb").write(bytes(data))


def test_corruption_is_found_where_the_design_promises(tmp_path):
    good = str(tmp_path / "good.kgl")
    _build().save(good)

    numeric = str(tmp_path / "numeric.kgl")
    open(numeric, "wb").write(open(good, "rb").read())
    _flip(numeric, b"col:Big:ratio:data", 100)
    graph = kglite.load(numeric, columns="mapped")  # numerics are checked lazily
    report = graph.verify_integrity()
    assert not report["ok"]
    assert [s["name"] for s in report["sections"] if s["error"]] == ["col:Big:ratio:data"]

    strings = str(tmp_path / "strings.kgl")
    open(strings, "wb").write(open(good, "rb").read())
    _flip(strings, b"col:Big:label:bytes", 10)
    for policy in ("mapped", "memory"):
        with pytest.raises(Exception, match="corrupt"):
            kglite.load(strings, columns=policy)


def _tmp_listing(root: str) -> set[str]:
    return {name for name in os.listdir(root) if name.startswith("kglite_portable_")}


def test_a_mapped_load_writes_no_spill_directory(tmp_path):
    # Every large column of this graph is incompressible as deltas, so a v9
    # save leaves none for the decode-and-spill path.
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        f"UNWIND range(1, {ROWS}) AS i CREATE (:Big {{id: (i * 7919) % 40009, "
        "title: 't' + toString(i), score: (i * 7919) % 40009 * 104729, ratio: i / 7.0}) "
    )
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    spill = tmp_path / "spill"
    spill.mkdir()
    script = textwrap.dedent(
        """
        import os, sys, kglite
        g = kglite.load(sys.argv[1], columns=sys.argv[2])
        print(len(os.listdir(sys.argv[3])), len([s for s in g.verify_integrity()["sections"]]))
        """
    )

    def run(policy):
        out = subprocess.run(
            [sys.executable, "-c", script, path, policy, str(spill)],
            capture_output=True,
            text=True,
            check=True,
            env={**os.environ, "KGLITE_TMPDIR": str(spill)},
        )
        return out.stdout.split()

    mapped_dirs, mapped_sections = run("mapped")
    memory_dirs, memory_sections = run("memory")
    assert mapped_dirs == "0" and int(mapped_sections) > 0
    assert memory_dirs == "1"  # a decoded load spills its large columns, as a v8 load does
    assert memory_sections == "0"


def test_two_processes_map_the_same_file_without_spilling(v9, tmp_path):
    spill = tmp_path / "spill"
    spill.mkdir()
    script = textwrap.dedent(
        """
        import os, sys, kglite
        g = kglite.load(sys.argv[1], columns="mapped")
        n = len([s for s in g.verify_integrity()["sections"] if s["mapped"]])
        print(n, g.cypher("MATCH (n:Big) RETURN count(n) AS c").to_list()[0]["c"])
        sys.stdout.flush()
        sys.stdin.readline()
        """
    )
    env = {**os.environ, "KGLITE_TMPDIR": str(spill)}
    procs = [
        subprocess.Popen(
            [sys.executable, "-c", script, v9],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            env=env,
        )
        for _ in range(2)
    ]
    try:
        lines = [p.stdout.readline().split() for p in procs]
        for sections, count in lines:
            assert int(sections) > 0 and int(count) == ROWS
        # Both processes are serving the pages of one file; neither wrote a
        # per-process copy. (The monotonic `seq` column is delta-compressed in
        # the file, so each process may have decoded that one.)
        mapped_dirs = [d for d in os.listdir(spill)]
        for d in mapped_dirs:
            files = os.listdir(spill / d) if (spill / d).is_dir() else []
            assert all(not f.endswith((".f64", ".str", ".off")) for f in files), (d, files)
    finally:
        for p in procs:
            p.communicate("\n", timeout=30)


def test_a_mapped_load_writes_no_bytes_where_a_decoded_one_writes_the_columns(tmp_path):
    """Footprint cannot tell the two apart (a spill file's clean pages are as
    invisible to macOS `phys_footprint` as a mapped file's); the disk writes can."""
    graph = kglite.KnowledgeGraph()
    graph.cypher(
        "UNWIND range(1, 1000000) AS i CREATE (:Big {id: (i * 7919) % 1000003, "
        "score: (i * 7919) % 1000003 * 104729, ratio: i / 7.0})"
    )
    path = str(tmp_path / "g.kgl")
    graph.save(path)
    script = textwrap.dedent(
        """
        import os, sys, kglite
        g = kglite.load(sys.argv[1], columns=sys.argv[2])
        total = 0
        for root, _, files in os.walk(sys.argv[3]):
            total += sum(os.path.getsize(os.path.join(root, f)) for f in files)
        print(total)
        """
    )

    def spilled(policy):
        spill = tmp_path / f"spill-{policy}"
        spill.mkdir()
        out = subprocess.run(
            [sys.executable, "-c", script, path, policy, str(spill)],
            capture_output=True,
            text=True,
            check=True,
            env={**os.environ, "KGLITE_TMPDIR": str(spill)},
        )
        return int(out.stdout.strip().splitlines()[-1])

    assert spilled("mapped") == 0
    assert spilled("memory") >= 1_000_000 * 8 * 3 * 0.9  # id, score, ratio at 8 bytes a row
