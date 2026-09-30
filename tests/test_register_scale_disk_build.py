"""A Pand-shaped register built in chunks into a disk graph, saved after every
chunk and reopened: pins today's footprint, reload cost and answers.

The footprint numbers are today's measurements (debug extension, macOS,
2026-09-30) with 30 % headroom. They are ceilings on a shape that is known to
be heavy, so later storage work tightens them. Only Darwin asserts them: the
Linux allocator has not been measured, so there the numbers are recorded in
the failure message of a functional assertion instead of gating.
"""

from __future__ import annotations

import gc
import sys
import warnings

import pandas as pd
import pytest

import kglite
from tests.fixtures.register_scale import ANCHOR_TYPE, REL, TYPE, chunk

psutil = pytest.importorskip("psutil")

CHUNKS = 4
CHUNK_VERSIONS = 50_000
TOTAL = CHUNKS * CHUNK_VERSIONS
HEADROOM = 1.3

# Metric: unique set size (psutil `memory_full_info().uss`) after `gc.collect()`
# and `kglite.trim_memory()`, as a delta over the same reading taken once the
# input frames exist and before the graph does. RSS is not used: it counts
# resident pages of the memory-mapped generation files, which the operating
# system reclaims at will. The figures are the worst seen across contexts (the
# file alone, after other disk tests, after the temporal tests); a long-lived
# process reads lower after saves because freed heap is reused, so these are
# ceilings, not typical values. The build slope, ~350 B per version, is what
# the storage work is meant to bring down.
MEASURED_SAVE_MB = (28.0, 39.0, 69.0, 72.0)  # after the save of chunk 0..3
MEASURED_RELOAD_MB = 99.0  # `kglite.load` of the saved directory, after dropping the writer (67 in the file alone)
ENFORCE_FOOTPRINT = sys.platform == "darwin"

AS_OF = ("2005-06-30T12:34:56.789012", "2020-01-01T00:00:00", "a stored valid_to")


def _uss_mb() -> float:
    gc.collect()
    kglite.trim_memory()
    return psutil.Process().memory_full_info().uss / 1e6


@pytest.fixture(scope="module")
def built(tmp_path_factory):
    path = tmp_path_factory.mktemp("register") / "pand"
    chunks = [chunk(i, CHUNK_VERSIONS) for i in range(CHUNKS)]
    base = _uss_mb()
    after_save: list[float] = []
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
        del graph
        before_reload = _uss_mb()
        reopened = kglite.load(str(path))
        reload_delta = _uss_mb() - before_reload
    frame = pd.concat([part.versions for part in chunks], ignore_index=True)
    return {"graph": reopened, "frame": frame, "after_save": after_save, "reload": reload_delta}


def _report(built) -> str:
    return f"after_save_mb={[round(x, 1) for x in built['after_save']]} reload_mb={built['reload']:.1f}"


def test_footprint_after_each_save_is_bounded(built):
    limits = [round(m * HEADROOM, 1) for m in MEASURED_SAVE_MB]
    if not ENFORCE_FOOTPRINT:
        pytest.skip(f"footprint ceilings are pinned on Darwin only; measured here: {_report(built)}")
    assert all(a <= lim for a, lim in zip(built["after_save"], limits, strict=True)), (
        f"{_report(built)} vs limits {limits}"
    )


def test_reload_delta_is_bounded(built):
    if not ENFORCE_FOOTPRINT:
        pytest.skip(f"footprint ceilings are pinned on Darwin only; measured here: {_report(built)}")
    assert built["reload"] <= MEASURED_RELOAD_MB * HEADROOM, _report(built)


def test_reopened_graph_has_every_row(built):
    graph, frame = built["graph"], built["frame"]
    assert graph.cypher(f"MATCH (p:{TYPE}) RETURN count(*) AS c").to_list() == [{"c": TOTAL}]
    assert graph.cypher(f"MATCH (o:{ANCHOR_TYPE}) RETURN count(*) AS c").to_list() == [{"c": frame["ident"].nunique()}]
    assert graph.cypher(f"MATCH (:{TYPE})-[:{REL}]->(:{ANCHOR_TYPE}) RETURN count(*) AS c").to_list() == [{"c": TOTAL}]


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
            f"MATCH (p:{TYPE} {{id: $i}}) RETURN p.title AS t, p.status AS s, p.bouwjaar AS b",
            params={"i": int(row.id)},
        ).to_list()
        assert got == [{"t": int(row.ident), "s": row.status, "b": int(row.bouwjaar)}]
    versions = frame.groupby("ident")["id"].apply(sorted)
    several = versions[versions.map(len) >= 3]
    ident = several.index[len(several) // 2]
    got = graph.cypher(
        f"MATCH (p:{TYPE} {{title: $t}}) RETURN p.id AS id ORDER BY id", params={"t": int(ident)}
    ).to_list()
    assert [r["id"] for r in got] == versions[ident]
    missing = int(frame["id"].max()) + 1
    assert graph.cypher(f"MATCH (p:{TYPE} {{id: $i}}) RETURN count(*) AS c", params={"i": missing}).to_list() == [
        {"c": 0}
    ]


def test_generator_is_deterministic_and_pand_shaped():
    a, b = chunk(3, 20_000), chunk(3, 20_000)
    assert a.versions.equals(b.versions) and a.edges.equals(b.edges)
    v = a.versions
    assert len(v) == 20_000 and v["id"].is_unique and v["id"].dtype == "int64" and v["bouwjaar"].dtype == "int32"
    assert 3.0e12 < v["ident"].min() and v["ident"].max() < 3.2e12 and 3.2e12 < v["id"].min()
    closed = v["valid_to"].notna()
    assert 0.35 < closed.mean() < 0.50
    assert (v.loc[closed, "valid_to"] > v.loc[closed, "valid_from"]).all()
    assert set(v["status"]) <= {"in_use", "under_construction", "permit_issued", "demolished", "not_realised"}
    assert set(a.anchors["id"]) == set(v["ident"]) and len(a.edges) == len(v)
    assert chunk(4, 20_000).versions["ident"].min() > v["ident"].max()
