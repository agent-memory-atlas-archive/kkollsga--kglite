"""A register of versioned records (employment history) built in chunks into a disk graph, saved after every
chunk and reopened: pins today's footprint, reload cost and answers.

The footprint numbers are measurements (debug extension, macOS, 2026-09-30)
with 30 % headroom. They are ceilings on a shape that is known to be heavy, so
later storage work tightens them; they were last tightened when the int64 id index began
to be served from the file a save wrote. Only Darwin asserts them: the
Linux allocator has not been measured, so there the numbers are recorded in
the failure message of a functional assertion instead of gating.
"""

from __future__ import annotations

import gc
import os
import sys
import warnings

import pandas as pd
import pytest

import kglite
from tests.fixtures.register_scale import ANCHOR_TYPE, REL, TYPE, chunk

try:
    import psutil
except ImportError:  # only the footprint arms need it; the oracle arms below must always run
    psutil = None

CHUNKS = 4
CHUNK_VERSIONS = 50_000
TOTAL = CHUNKS * CHUNK_VERSIONS
HEADROOM = 1.3

# Metrics. Both are a delta over the same reading taken once the input frames
# exist and before the graph does, after `gc.collect()` + `kglite.trim_memory()`.
#
# * unique set size (psutil `memory_full_info().uss`): the process's private
#   resident pages. It counts the clean pages of memory-mapped generation files
#   that the process has touched, which the operating system reclaims at will,
#   so it under-reads what moving columns onto files saves.
# * `phys_footprint` (`proc_pid_rusage`, macOS only): what the OS charges the
#   process, which excludes clean file-backed pages. It is the figure the
#   register-scale promise is stated in, and it shows the storage work.
#
# The figures are the worst seen across contexts (the file alone, after other
# disk tests, after the temporal tests); a long-lived process reads lower after
# saves because freed heap is reused, so these are ceilings, not typical values.
# History of the same fixture on the same metrics (debug, macOS): before the
# column files were re-pointed, `phys_footprint` read (26, 33, 46, 43) MB after the
# saves and 71 MB after the reload; then (12, 22, 21, 29) and 31, where the id
# index (a heap map until it is served from a file) was most of what was left;
# now the id index is a 12-byte-per-id array searched in the mapping and the
# reload adds nothing `phys_footprint` charges. USS still counts the mapped
# pages the reload and the lookups touched.
MEASURED_SAVE_MB = (18.4, 30.7, 43.7, 56.8)  # USS after the save of chunk 0..3
MEASURED_RELOAD_MB = 37.9  # USS: `kglite.load` of the saved directory, after dropping the writer
MEASURED_PHYS_SAVE_MB = (9.6, 11.9, 15.4, 19.2)  # phys_footprint after the save of chunk 0..3
# `phys_footprint` after `kglite.load` reads 0.0 in every context: below the
# allocator's resolution, so the pin is a 3 MB noise floor, not a measurement.
MEASURED_PHYS_RELOAD_MB = 3.0
ENFORCE_FOOTPRINT = sys.platform == "darwin"

AS_OF = ("2005-06-30T12:34:56.789012", "2020-01-01T00:00:00", "a stored valid_to")


def _phys_mb() -> float:
    """`phys_footprint` in MB (macOS only; NaN elsewhere)."""
    gc.collect()
    kglite.trim_memory()
    if sys.platform != "darwin":
        return float("nan")
    import ctypes

    usage = (ctypes.c_uint64 * 40)()
    ctypes.CDLL("/usr/lib/libproc.dylib").proc_pid_rusage(os.getpid(), 2, ctypes.byref(usage))
    return usage[9] / 1e6  # ri_phys_footprint in `struct rusage_info_v2`


def _uss_mb() -> float:
    """Unique set size in MB, or NaN when psutil is not installed."""
    gc.collect()
    kglite.trim_memory()
    if psutil is None:
        return float("nan")
    return psutil.Process().memory_full_info().uss / 1e6


@pytest.fixture(scope="module")
def built(tmp_path_factory):
    path = tmp_path_factory.mktemp("register") / "employment"
    chunks = [chunk(i, CHUNK_VERSIONS) for i in range(CHUNKS)]
    base, phys_base = _uss_mb(), _phys_mb()
    after_save: list[float] = []
    phys_after_save: list[float] = []
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        graph = kglite.KnowledgeGraph(storage="disk", path=str(path))
        for i, part in enumerate(chunks):
            graph.add_nodes(part.versions, TYPE, "id", "ident")
            if i == 0:
                graph.set_temporal(TYPE, "valid_from", "valid_to", convention="half_open")
            graph.add_nodes(part.anchors, ANCHOR_TYPE, "id", conflict_handling="skip")
            graph.add_relationships(part.edges, REL, TYPE, "id", ANCHOR_TYPE, "ident")
            graph.save()
            after_save.append(_uss_mb() - base)
            phys_after_save.append(_phys_mb() - phys_base)
        del graph
        before_reload, phys_before_reload = _uss_mb(), _phys_mb()
        reopened = kglite.load(str(path))
        reload_delta = _uss_mb() - before_reload
        phys_reload_delta = _phys_mb() - phys_before_reload
    frame = pd.concat([part.versions for part in chunks], ignore_index=True)
    return {
        "graph": reopened,
        "frame": frame,
        "after_save": after_save,
        "reload": reload_delta,
        "phys_after_save": phys_after_save,
        "phys_reload": phys_reload_delta,
    }


def _report(built) -> str:
    return (
        f"after_save_mb={[round(x, 1) for x in built['after_save']]} reload_mb={built['reload']:.1f} "
        f"phys_after_save_mb={[round(x, 1) for x in built['phys_after_save']]} "
        f"phys_reload_mb={built['phys_reload']:.1f}"
    )


def _require_footprint(built) -> None:
    if psutil is None:
        pytest.skip("psutil is not installed; the footprint arms need it (the answer arms do not)")
    if not ENFORCE_FOOTPRINT:
        pytest.skip(f"footprint ceilings are pinned on Darwin only; measured here: {_report(built)}")


def test_footprint_after_each_save_is_bounded(built):
    limits = [round(m * HEADROOM, 1) for m in MEASURED_SAVE_MB]
    phys_limits = [round(m * HEADROOM, 1) for m in MEASURED_PHYS_SAVE_MB]
    _require_footprint(built)
    assert all(a <= lim for a, lim in zip(built["after_save"], limits, strict=True)), (
        f"{_report(built)} vs limits {limits}"
    )
    assert all(a <= lim for a, lim in zip(built["phys_after_save"], phys_limits, strict=True)), (
        f"{_report(built)} vs phys limits {phys_limits}"
    )


def test_reload_delta_is_bounded(built):
    _require_footprint(built)
    assert built["reload"] <= MEASURED_RELOAD_MB * HEADROOM, _report(built)
    assert built["phys_reload"] <= MEASURED_PHYS_RELOAD_MB * HEADROOM, _report(built)


def test_reopened_graph_has_every_row(built):
    graph, frame = built["graph"], built["frame"]
    assert graph.cypher(f"FOR VALID_TIME ALL MATCH (p:{TYPE}) RETURN count(*) AS c").to_list() == [{"c": TOTAL}]
    assert graph.cypher(f"FOR VALID_TIME ALL MATCH (o:{ANCHOR_TYPE}) RETURN count(*) AS c").to_list() == [
        {"c": frame["ident"].nunique()}
    ]
    assert graph.cypher(
        f"FOR VALID_TIME ALL MATCH (:{TYPE})-[:{REL}]->(:{ANCHOR_TYPE}) RETURN count(*) AS c"
    ).to_list() == [{"c": TOTAL}]


@pytest.mark.parametrize("instant", AS_OF)
def test_as_of_count_matches_pandas(built, instant):
    frame = built["frame"]
    if instant == "a stored valid_to":  # half-open: the instant a version closes it is already gone
        at = frame["valid_to"].dropna().iloc[1_000]
        instant = at.isoformat()
        assert (frame["valid_to"] == at).any()
    else:
        at = pd.Timestamp(instant)
    expected = int(((frame["valid_from"] <= at) & (frame["valid_to"].isna() | (frame["valid_to"] > at))).sum())
    assert 0 < expected < TOTAL
    query = f"FOR VALID_TIME AS OF datetime('{instant}') MATCH (p:{TYPE}) RETURN count(*) AS c"
    assert built["graph"].cypher(query).to_list() == [{"c": expected}]


def test_lookup_by_id_and_by_title(built):
    graph, frame = built["graph"], built["frame"]
    for position in (0, CHUNK_VERSIONS - 1, CHUNK_VERSIONS, 123_457, TOTAL - 1):
        row = frame.iloc[position]
        got = graph.cypher(
            f"FOR VALID_TIME ALL MATCH (p:{TYPE} {{id: $i}}) RETURN p.title AS t, p.status AS s, p.hire_year AS b",
            params={"i": int(row.id)},
        ).to_list()
        assert got == [{"t": int(row.ident), "s": row.status, "b": int(row.hire_year)}]
    versions = frame.groupby("ident")["id"].apply(sorted)
    several = versions[versions.map(len) >= 3]
    ident = several.index[len(several) // 2]
    got = graph.cypher(
        f"FOR VALID_TIME ALL MATCH (p:{TYPE} {{title: $t}}) RETURN p.id AS id ORDER BY id", params={"t": int(ident)}
    ).to_list()
    assert [r["id"] for r in got] == versions[ident]
    missing = int(frame["id"].max()) + 1
    assert graph.cypher(
        f"FOR VALID_TIME ALL MATCH (p:{TYPE} {{id: $i}}) RETURN count(*) AS c", params={"i": missing}
    ).to_list() == [{"c": 0}]


def test_generator_is_deterministic_and_pand_shaped():
    a, b = chunk(3, 20_000), chunk(3, 20_000)
    assert a.versions.equals(b.versions) and a.edges.equals(b.edges)
    v = a.versions
    assert len(v) == 20_000 and v["id"].is_unique and v["id"].dtype == "int64" and v["hire_year"].dtype == "int32"
    assert 3.0e12 < v["ident"].min() and v["ident"].max() < 3.2e12 and 3.2e12 < v["id"].min()
    closed = v["valid_to"].notna()
    assert 0.35 < closed.mean() < 0.50
    assert (v.loc[closed, "valid_to"] > v.loc[closed, "valid_from"]).all()
    assert set(v["status"]) <= {"active", "onboarding", "offer_made", "terminated", "withdrawn"}
    assert set(a.anchors["id"]) == set(v["ident"]) and len(a.edges) == len(v)
    assert chunk(4, 20_000).versions["ident"].min() > v["ident"].max()
