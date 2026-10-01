"""A reopened disk graph keeps its serving shape across saves.

Every re-save of a reopened disk graph — a write or none — used to move every
type out of its mmap-served column file (once a shared ``seg_000/columns.bin``,
now one ``seg_000/type_columns/<key>.bin`` per type) into per-type
``columns/<type>/columns.zst`` sidecars whose every column was ``Mixed``. The
next load decoded those onto the heap (55 → ~330 B/row) and every later save
kept them there. Each cycle here reopens in a fresh process, writes, saves,
and checks the generation's files, the values, and (macOS) the reload
footprint against the first cycle's.
"""

import json
import os
import subprocess
import sys
import textwrap

import pytest

from tests.fixtures.disk_generation import current_generation

ROWS = 100_000
OTHER = ROWS // 4

BUILD = textwrap.dedent(
    """
    import sys
    import numpy as np, pandas as pd, kglite
    path, n = sys.argv[1], int(sys.argv[2])
    rng = np.random.default_rng(3)
    ids = np.arange(n, dtype="int64") + 10**6
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.add_nodes(pd.DataFrame({
        "id": ids, "name": ids.astype(str),
        "vk": rng.integers(1, 9, n).astype("int64"), "score": rng.random(n),
        "rec_to": np.zeros(n, dtype="int64"), "status": rng.choice(["a", "b", "c"], n),
    }), "Employment", "id", "name")
    m = n // 4
    ids2 = np.arange(m, dtype="int64") + 10**8
    g.add_nodes(pd.DataFrame({"id": ids2, "name": ids2.astype(str), "v": ids2}), "Other", "id", "name")
    g.save()
    """
)

CYCLE = textwrap.dedent(
    """
    import ctypes, gc, json, os, sys
    import kglite
    path, cycle, mode = sys.argv[1], int(sys.argv[2]), sys.argv[3]

    def footprint():
        if sys.platform != "darwin":
            return 0
        gc.collect()
        kglite.trim_memory()
        buf = (ctypes.c_uint64 * 40)()
        ctypes.CDLL("/usr/lib/libproc.dylib").proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf))
        return int(buf[9])

    before = footprint()
    g = kglite.load(path)
    reload_bytes = footprint() - before
    ids = [10**6 + i for i in range(100)]
    if mode == "set":
        g.cypher("UNWIND $ids AS i MATCH (n:Employment {id: i}) SET n.rec_to = $v",
                 params={"ids": ids, "v": cycle}).to_list()
    g.save()
    rec = g.cypher("MATCH (n:Employment) WHERE n.id IN [$a, $b] RETURN n.rec_to AS r ORDER BY n.id",
                   params={"a": ids[0], "b": 10**6 + 500}).to_list()
    other = g.cypher("MATCH (n:Other {id: $i}) RETURN n.v AS v", params={"i": 10**8 + 7}).to_list()
    title = g.cypher("MATCH (n:Employment {id: $i}) RETURN n.title AS t", params={"i": ids[5]}).to_list()
    print(json.dumps({"reload": reload_bytes, "rec": [r["r"] for r in rec],
                      "other": other[0]["v"], "title": title[0]["t"]}))
    """
)


def _run(script, *args):
    out = subprocess.run([sys.executable, "-c", script, *map(str, args)], check=True, capture_output=True, text=True)
    return out.stdout.strip().splitlines()[-1] if out.stdout.strip() else ""


@pytest.mark.parametrize("mode", ["set", "noop"])
def test_reopened_disk_graph_stays_in_its_column_files_across_saves(tmp_path, mode):
    path = str(tmp_path / "g")
    _run(BUILD, path, ROWS)
    reloads = []
    for cycle in (1, 2, 3):
        result = json.loads(_run(CYCLE, path, cycle, mode))
        generation = current_generation(path)
        with open(os.path.join(generation, "seg_000", "columns_meta.json"), encoding="utf-8") as handle:
            files = json.load(handle)["files"]
        assert sorted(files) == ["Employment", "Other"], f"cycle {cycle}: every type has its own column file: {files}"
        for name, relative in files.items():
            assert os.path.isfile(os.path.join(generation, "seg_000", relative)), f"cycle {cycle}: {name} -> {relative}"
        assert not os.path.exists(os.path.join(generation, "seg_000", "columns.bin")), f"cycle {cycle}"
        sidecars = [os.path.join(dp, f) for dp, _, fs in os.walk(os.path.join(generation, "columns")) for f in fs]
        assert sidecars == [], f"cycle {cycle}: types moved to heap sidecars: {sidecars}"
        expected_rec = cycle if mode == "set" else 0
        assert result["rec"] == [expected_rec, 0], f"cycle {cycle}"
        assert result["other"] == 10**8 + 7
        assert result["title"] == str(10**6 + 5)
        reloads.append(result["reload"])
    if sys.platform == "darwin":
        # The reload before cycle 1 reads the build's own save; later ones read
        # a re-save. A drifted re-save reloads at ~6x the first.
        assert max(reloads[1:]) <= 1.3 * reloads[0] + 1_000_000, (
            f"reload footprint per cycle {reloads} bytes for {ROWS + OTHER} rows"
        )
