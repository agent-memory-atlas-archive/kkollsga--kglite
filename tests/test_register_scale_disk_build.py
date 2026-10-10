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

import json
from pathlib import Path
import subprocess
import sys
import warnings

import pandas as pd
import pytest

import kglite
from tests.fixtures.register_scale import ANCHOR_TYPE, REL, TYPE, chunk

CHUNKS = 4
CHUNK_VERSIONS = 50_000
TOTAL = CHUNKS * CHUNK_VERSIONS
HEADROOM = 1.3

# Metrics. Both are a delta over a reading taken in a fresh child process once
# the input frames exist (and have been read) and before the graph does, after
# `gc.collect()` + `kglite.trim_memory()`. The child
# (`tests/fixtures/register_scale_footprint.py`) is why the readings do not
# depend on what ran before in the pytest process, or on pages of it that the
# operating system swapped out and faulted back mid-build.
#
# * unique set size (psutil `memory_full_info().uss`): the process's private
#   resident pages. It counts the clean pages of memory-mapped generation files
#   that the process has touched, which the operating system reclaims at will,
#   so it under-reads what moving columns onto files saves.
# * `phys_footprint` (`proc_pid_rusage`, macOS only): what the OS charges the
#   process, which excludes clean file-backed pages. It is the figure the
#   register-scale promise is stated in, and it shows the storage work.
#
# The figures are the worst of 12 child runs (debug, macOS, 2026-10-10, machine
# under load; run-to-run spread 2.4 MB on `phys_footprint`). A fresh process
# reads higher than the long-lived pytest process the earlier pins came from,
# because it has no freed heap to reuse. History of the same fixture on
# `phys_footprint` (debug, macOS, pytest process): before the
# column files were re-pointed it read (26, 33, 46, 43) MB after the
# saves and 71 MB after the reload; then (12, 22, 21, 29) and 31, where the id
# index (a heap map until it is served from a file) was most of what was left;
# now the id index is a 12-byte-per-id array searched in the mapping and the
# reload adds nothing `phys_footprint` charges. USS still counts the mapped
# pages the reload and the lookups touched.
MEASURED_SAVE_MB = (23.2, 35.6, 48.5, 61.1)  # USS after the save of chunk 0..3
MEASURED_RELOAD_MB = 37.8  # USS: `kglite.load` of the saved directory, after dropping the writer
MEASURED_PHYS_SAVE_MB = (14.3, 16.8, 20.3, 23.6)  # phys_footprint after the save of chunk 0..3
# `phys_footprint` after `kglite.load` reads 0.0 in every context: below the
# allocator's resolution, so the pin is a 3 MB noise floor, not a measurement.
MEASURED_PHYS_RELOAD_MB = 3.0
ENFORCE_FOOTPRINT = sys.platform == "darwin"

AS_OF = ("2005-06-30T12:34:56.789012", "2020-01-01T00:00:00", "a stored valid_to")


@pytest.fixture(scope="module")
def footprint(tmp_path_factory):
    """The footprint readings, taken in a child process; see the probe's docstring."""
    directory = tmp_path_factory.mktemp("register-footprint")
    done = subprocess.run(
        [
            sys.executable,
            "-m",
            "tests.fixtures.register_scale_footprint",
            str(directory),
            str(CHUNKS),
            str(CHUNK_VERSIONS),
        ],
        capture_output=True,
        text=True,
        timeout=110,
        cwd=Path(__file__).resolve().parent.parent,
    )
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout.strip().splitlines()[-1])


@pytest.fixture(scope="module")
def built(tmp_path_factory):
    path = tmp_path_factory.mktemp("register") / "employment"
    chunks = [chunk(i, CHUNK_VERSIONS) for i in range(CHUNKS)]
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
        del graph
        reopened = kglite.load(str(path))
    frame = pd.concat([part.versions for part in chunks], ignore_index=True)
    return {"graph": reopened, "frame": frame}


def _report(footprint) -> str:
    return (
        f"after_save_mb={[round(x, 1) for x in footprint['after_save']]} reload_mb={footprint['reload']:.1f} "
        f"phys_after_save_mb={[round(x, 1) for x in footprint['phys_after_save']]} "
        f"phys_reload_mb={footprint['phys_reload']:.1f}"
    )


def _require_footprint(footprint) -> None:
    if all(x != x for x in footprint["after_save"]):  # NaN: the probe had no psutil
        pytest.skip("psutil is not installed; the footprint arms need it (the answer arms do not)")
    if not ENFORCE_FOOTPRINT:
        pytest.skip(f"footprint ceilings are pinned on Darwin only; measured here: {_report(footprint)}")


def test_footprint_after_each_save_is_bounded(footprint):
    limits = [round(m * HEADROOM, 1) for m in MEASURED_SAVE_MB]
    phys_limits = [round(m * HEADROOM, 1) for m in MEASURED_PHYS_SAVE_MB]
    _require_footprint(footprint)
    assert all(a <= lim for a, lim in zip(footprint["after_save"], limits, strict=True)), (
        f"{_report(footprint)} vs limits {limits}"
    )
    assert all(a <= lim for a, lim in zip(footprint["phys_after_save"], phys_limits, strict=True)), (
        f"{_report(footprint)} vs phys limits {phys_limits}"
    )


def test_reload_delta_is_bounded(footprint):
    _require_footprint(footprint)
    assert footprint["reload"] <= MEASURED_RELOAD_MB * HEADROOM, _report(footprint)
    assert footprint["phys_reload"] <= MEASURED_PHYS_RELOAD_MB * HEADROOM, _report(footprint)


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
