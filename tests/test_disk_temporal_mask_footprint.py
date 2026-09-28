"""The Disk instant mask costs its bits, not a copy of every relationship.

Building the mask walks every relationship once. Reading each one's type
through its materialised record parked an owned copy of its properties for the
whole walk (~140 B per relationship: 28 MB at 200 k), against a mask of one
bit per slot. The walk now reads the type from the CSR record and the bounds
through the property store, so its peak growth stays near the mask size.

Footprint is read with `proc_pid_rusage` (macOS only), in a fresh process so
the build's freed pages cannot absorb the growth.
"""

import json
import subprocess
import sys
import textwrap

import pytest

pytestmark = pytest.mark.skipif(sys.platform != "darwin", reason="phys_footprint via libproc is macOS-only")

NODES = 20_000
EDGES = 200_000

BUILD = textwrap.dedent(
    """
    import datetime as dt, sys
    import numpy as np, pandas as pd, kglite
    path, n, m = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    rng = np.random.default_rng(5)
    ids = np.arange(n, dtype="int64")
    valid = rng.random(n) < 0.01
    g = kglite.KnowledgeGraph(storage="disk", path=path)
    g.add_nodes(pd.DataFrame({
        "id": ids, "name": ids.astype(str),
        "vf": pd.Series([dt.date(2000, 1, 1)] * n),
        "vt": pd.Series([None if v else dt.date(2001, 1, 1) for v in valid], dtype=object),
    }), "V", "id", "name", column_types={"vf": "date", "vt": "date"})
    evalid = rng.random(m) < 0.5
    g.add_relationships(pd.DataFrame({
        "s": rng.integers(0, n, m), "t": rng.integers(0, n, m),
        "lf": [dt.date(2000, 1, 1)] * m,
        "lt": pd.Series([None if v else dt.date(2001, 1, 1) for v in evalid], dtype=object),
    }), "L", "V", "s", "V", "t", column_types={"lf": "date", "lt": "date"})
    g.set_temporal("V", "vf", "vt")
    g.cypher("CALL db.temporal.declare({relationship: 'L', from: 'lf', to: 'lt', "
             "convention: 'closed'})").to_list()
    g.save()
    """
)

MEASURE = textwrap.dedent(
    """
    import ctypes, datetime as dt, gc, json, os, sys, threading, time
    import kglite
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")

    def footprint():
        buf = (ctypes.c_uint64 * 40)()
        libproc.proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf))
        return int(buf[9])  # ri_phys_footprint

    g = kglite.load(sys.argv[1])
    edges = g.cypher("MATCH ()-[r:L]->() RETURN count(r) AS c").to_list()[0]["c"]
    gc.collect()
    kglite.trim_memory()
    base = footprint()
    peak = [base]
    stop = threading.Event()

    def sample():
        while not stop.is_set():
            peak[0] = max(peak[0], footprint())
            time.sleep(0.0005)

    sampler = threading.Thread(target=sample, daemon=True)
    sampler.start()
    rows = g.cypher(
        "FOR VALID_TIME AS OF $t CALL degree() YIELD node RETURN count(*) AS n",
        params={"t": dt.date(2007, 6, 30)},
    ).to_list()
    stop.set()
    sampler.join()
    print(json.dumps({"edges": edges, "rows": rows[0]["n"], "growth": peak[0] - base}))
    """
)


def test_disk_instant_mask_peak_growth_is_near_the_mask_size(tmp_path):
    path = str(tmp_path / "g")
    subprocess.run([sys.executable, "-c", BUILD, path, str(NODES), str(EDGES)], check=True)
    out = subprocess.run([sys.executable, "-c", MEASURE, path], check=True, capture_output=True, text=True)
    result = json.loads(out.stdout.strip().splitlines()[-1])
    assert result["edges"] == EDGES
    # Non-vacuity: the as-of call ran over the masked graph (~1% of V valid).
    assert 0 < result["rows"] < NODES // 10
    mask_bytes = (NODES + EDGES + 7) // 8
    limit = mask_bytes + 4 * 1024 * 1024
    assert result["growth"] <= limit, (
        f"peak footprint grew {result['growth'] / 1e6:.1f} MB during a masked CALL degree() over "
        f"{EDGES} relationships; the mask is {mask_bytes} bytes. ~140 B per relationship is the "
        "mask walk parking a materialised copy of every relationship."
    )
