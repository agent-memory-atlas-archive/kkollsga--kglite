"""A filtered ``MATCH … WHERE … RETURN … LIMIT k`` holds about ``k`` rows, not
every match, in every storage mode.

A surviving WHERE kept the matcher uncapped, so the executor materialised every
match of the type before filtering and stopping: ~670 MB to return 12 000 rows
of a 1M-node type. Now a WHERE the pattern already enforces is dropped (the
scan stops at the limit), and a residual WHERE drains the matcher a slice at a
time. Each query runs in a child process that samples its own resident size, so
the measurement does not depend on the query releasing the GIL. Only Darwin
asserts the ceiling, as in the other footprint tests.
"""

import json
import subprocess
import sys
import textwrap

import pytest

ROWS = 300_000
CEILING_MB = 10

CHILD = textwrap.dedent(
    """
    import ctypes, json, os, sys, threading, time
    import numpy as np, pandas as pd
    import kglite

    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")

    def footprint():
        # proc_pid_rusage(RUSAGE_INFO_V2).ri_phys_footprint: what Activity
        # Monitor reports; RSS misses pages the allocator reuses.
        buf = (ctypes.c_uint64 * 40)()
        libproc.proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf))
        return int(buf[9])

    storage, path, rows = sys.argv[1], sys.argv[2], int(sys.argv[3])
    g = kglite.KnowledgeGraph(storage="disk", path=path) if storage == "disk" else kglite.KnowledgeGraph()
    rng = np.random.default_rng(1)
    g.add_nodes(
        pd.DataFrame({
            "id": np.arange(rows),
            "status": np.where(rng.random(rows) < 0.5, "a", "b"),
            "closed": pd.Series(
                np.where(rng.random(rows) < 0.3, pd.Timestamp("2001-01-01"), pd.NaT),
                dtype="datetime64[us]",
            ),
        }),
        "Pand",
        "id",
    )
    peak = [0]
    stop = [False]

    def sample():
        while not stop[0]:
            peak[0] = max(peak[0], footprint())
            time.sleep(0.001)

    threading.Thread(target=sample, daemon=True).start()
    out = {}
    for name, query in [
        ("subsumed", "MATCH (p:Pand) WHERE p.status = 'a' RETURN p.id AS id LIMIT 500"),
        ("range", "MATCH (p:Pand) WHERE p.id > 5 RETURN p.id AS id LIMIT 500"),
        ("residual", "MATCH (p:Pand) WHERE p.closed IS NULL RETURN p.id AS id LIMIT 500"),
    ]:
        kglite.trim_memory()
        time.sleep(0.05)
        base = footprint()
        peak[0] = base
        n = len(g.cypher(query).to_list())
        time.sleep(0.01)
        out[name] = {"rows": n, "delta_mb": (peak[0] - base) / 1e6}
    stop[0] = True
    print(json.dumps(out), flush=True)
    """
)


@pytest.mark.skipif(sys.platform != "darwin", reason="footprint ceiling is calibrated on Darwin")
@pytest.mark.parametrize("storage", ["memory", "disk"])
def test_filtered_limit_holds_the_limit_not_every_match(storage, tmp_path):
    result = subprocess.run(
        [sys.executable, "-c", CHILD, storage, str(tmp_path / "g"), str(ROWS)],
        capture_output=True,
        text=True,
        timeout=110,
        check=True,
    )
    measured = json.loads(result.stdout.strip().splitlines()[-1])
    for name, cell in measured.items():
        assert cell["rows"] == 500, (name, cell)
        assert cell["delta_mb"] < CEILING_MB, (storage, name, cell)
