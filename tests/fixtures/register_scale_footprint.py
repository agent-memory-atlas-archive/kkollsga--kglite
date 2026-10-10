"""Footprint probe for ``tests/test_register_scale_disk_build.py``: builds the
register in chunks into a disk graph, saving after every chunk, and reports the
process's footprint after each save and after a reload as one JSON line.

It runs in a fresh child process so the readings carry nothing from the pytest
process around it: a long-lived test process holds hundreds of MB of unrelated
pages that the operating system may swap out and fault back during a build,
and every re-faulted page reads as growth in a unique-set-size delta. The
input frames are read once (hashed) before the baseline for the same reason.

Run as ``python -m tests.fixtures.register_scale_footprint <dir> <chunks> <versions>``.
"""

from __future__ import annotations

import gc
import json
import os
import sys
import time
import warnings

import pandas as pd

import kglite
from tests.fixtures.register_scale import ANCHOR_TYPE, REL, TYPE, chunk

try:
    import psutil
except ImportError:
    psutil = None


def phys_mb() -> float:
    """`phys_footprint` in MB (macOS only; NaN elsewhere)."""
    gc.collect()
    kglite.trim_memory()
    if sys.platform != "darwin":
        return float("nan")
    import ctypes

    usage = (ctypes.c_uint64 * 40)()
    ctypes.CDLL("/usr/lib/libproc.dylib").proc_pid_rusage(os.getpid(), 2, ctypes.byref(usage))
    return usage[9] / 1e6  # ri_phys_footprint in `struct rusage_info_v2`


def uss_mb() -> float:
    """Unique set size in MB, or NaN when psutil is not installed."""
    gc.collect()
    kglite.trim_memory()
    if psutil is None:
        return float("nan")
    return psutil.Process().memory_full_info().uss / 1e6


def main(directory: str, chunk_count: int, chunk_versions: int) -> dict:
    chunks = [chunk(i, chunk_versions) for i in range(chunk_count)]
    for part in chunks:  # fault every input page in before the baseline
        for frame in (part.versions, part.anchors, part.edges):
            pd.util.hash_pandas_object(frame, index=False)
    time.sleep(0.2)
    base, phys_base = uss_mb(), phys_mb()
    after_save: list[float] = []
    phys_after_save: list[float] = []
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=os.path.join(directory, "employment"))
        for i, part in enumerate(chunks):
            graph.add_nodes(part.versions, TYPE, "id", "ident")
            if i == 0:
                graph.set_temporal(TYPE, "valid_from", "valid_to", convention="half_open")
            graph.add_nodes(part.anchors, ANCHOR_TYPE, "id", conflict_handling="skip")
            graph.add_relationships(part.edges, REL, TYPE, "id", ANCHOR_TYPE, "ident")
            graph.save()
            after_save.append(uss_mb() - base)
            phys_after_save.append(phys_mb() - phys_base)
        del graph
        before_reload, phys_before_reload = uss_mb(), phys_mb()
        reopened = kglite.load(os.path.join(directory, "employment"))
        reload_delta = uss_mb() - before_reload
        phys_reload_delta = phys_mb() - phys_before_reload
    del reopened
    return {
        "after_save": after_save,
        "reload": reload_delta,
        "phys_after_save": phys_after_save,
        "phys_reload": phys_reload_delta,
    }


if __name__ == "__main__":
    print(json.dumps(main(sys.argv[1], int(sys.argv[2]), int(sys.argv[3]))))
