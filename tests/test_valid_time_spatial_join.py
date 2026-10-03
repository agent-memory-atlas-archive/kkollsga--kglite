"""The fused spatial ``contains`` join under ``FOR VALID_TIME AS OF``.

The join builds an R-tree over the containers of one type and probes it with
every node of the other, so it is only correct when both sides drop the nodes
the context hides. The fixture puts a hidden container and a hidden probe at
the same location as a visible one, so an unmasked side shows up as an extra
pair at some instant. Both shapes (the single-MATCH location join and the
two-MATCH centroid join) must answer as the same statement with the pass
disabled, at twelve instants and under ALL.
"""

from __future__ import annotations

import pandas as pd
import pytest

import kglite

PASS = "fuse_spatial_join"

INSTANTS = [
    "2000-01-01",
    "2002-06-15",
    "2004-12-31",
    "2005-01-01",
    "2005-06-15",
    "2006-01-01",
    "2007-06-15",
    "2008-01-01",
    "2008-01-02",
    "2010-01-01",
    "2010-01-02",
    "2031-01-01",
]

CONTEXTS = [f"FOR VALID_TIME AS OF date('{d}') " for d in INSTANTS] + ["FOR VALID_TIME ALL "]


@pytest.fixture(scope="module")
def sites():
    graph = kglite.KnowledgeGraph()
    # Zones 1 and 2 cover the same box (hidden duplicates at the same place);
    # zone 3 covers a second box and is always visible.
    box = "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"
    far = "POLYGON((20 20, 30 20, 30 30, 20 30, 20 20))"
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2, 3], "title": ["zoneA", "zoneB", "zoneC"], "geometry": [box, box, far]}),
        "Zone",
        "id",
        "title",
        column_types={"geometry": "geometry"},
    )
    # Sites 1 and 2 sit at the same point; site 3 is always visible.
    graph.add_nodes(
        pd.DataFrame(
            {
                "id": [1, 2, 3],
                "title": ["site1", "site2", "site3"],
                "latitude": [5.0, 5.0, 25.0],
                "longitude": [5.0, 5.0, 25.0],
            }
        ),
        "Site",
        "id",
        "title",
        column_types={"latitude": "location.lat", "longitude": "location.lon"},
    )
    # Parcels are polygons whose centroid is the probe point.
    tiny = "POLYGON((4 4, 6 4, 6 6, 4 6, 4 4))"
    far_tiny = "POLYGON((24 24, 26 24, 26 26, 24 26, 24 24))"
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2, 3], "title": ["parcel1", "parcel2", "parcel3"], "geometry": [tiny, tiny, far_tiny]}),
        "Parcel",
        "id",
        "title",
        column_types={"geometry": "geometry"},
    )
    # Pin 2 carries a second label that is declared: the mask must honour it.
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2], "title": ["pin1", "pin2"], "latitude": [5.0, 5.0], "longitude": [5.0, 5.0]}),
        "Pin",
        "id",
        "title",
        column_types={"latitude": "location.lat", "longitude": "location.lon"},
    )
    # A geometry type that is also a second label on a located node: the
    # existing multi-label bail keeps the join off the pass.
    graph.add_nodes(
        pd.DataFrame({"id": [1], "title": ["region1"], "geometry": [box]}),
        "Region",
        "id",
        "title",
        column_types={"geometry": "geometry"},
    )
    graph.add_nodes(
        pd.DataFrame({"id": [1, 2], "title": ["tag1", "tag2"], "latitude": [5.0, 5.0], "longitude": [5.0, 5.0]}),
        "Tag",
        "id",
        "title",
        column_types={"latitude": "location.lat", "longitude": "location.lon"},
    )
    graph.cypher("MATCH (t:Tag {id: 2}) SET t:Region").to_list()
    windows = {
        "Zone": {1: ("2000-01-01", "2010-01-01"), 2: ("2005-01-01", None), 3: (None, None)},
        "Site": {1: ("2000-01-01", "2008-01-01"), 2: ("2006-01-01", None), 3: (None, None)},
        "Parcel": {1: ("2000-01-01", "2005-01-01"), 2: ("2005-06-15", None), 3: (None, None)},
        "Pin": {1: ("2000-01-01", None), 2: ("2000-01-01", None)},
    }
    for label, rows in windows.items():
        for node_id, (start, end) in rows.items():
            sets = [f"n.vf = date('{start or '1990-01-01'}')"]
            if end:
                sets.append(f"n.vt = date('{end}')")
            else:
                sets.append("n.vt = null")
            graph.cypher(f"MATCH (n:{label} {{id: {node_id}}}) SET {', '.join(sets)}").to_list()
    graph.cypher("MATCH (n:Pin {id: 2}) SET n:Flagged, n.fv = date('2004-01-01'), n.ft = date('2006-01-01')").to_list()
    for declaration in (
        "{node: 'Zone', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{node: 'Site', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Parcel', from: 'vf', to: 'vt', convention: 'half_open'}",
        "{node: 'Pin', from: 'vf', to: 'vt', convention: 'closed'}",
        "{node: 'Flagged', from: 'fv', to: 'ft', convention: 'closed'}",
    ):
        graph.cypher(f"CALL db.temporal.declare({declaration})").to_list()
    return graph


SINGLE = "MATCH (z:Zone), (s:Site) WHERE contains(z, s) RETURN z.title AS z, s.title AS s"
CENTROID = "MATCH (p:Parcel) MATCH (z:Zone) WHERE contains(z, centroid(p)) RETURN p.title AS p, z.title AS z"

# Each is a different way the join is written; all fuse.
FUSED = [
    SINGLE,
    "MATCH (s:Site), (z:Zone) WHERE contains(z, s) RETURN z.title AS z, s.title AS s",
    "MATCH (z:Zone), (s:Site) WHERE contains(z, s) AND s.title <> 'site3' RETURN z.title AS z, s.title AS s",
    "MATCH (z:Zone), (s:Site) WHERE contains(z, s) RETURN z.title AS z, count(s) AS n",
    "MATCH (z:Zone), (p:Pin) WHERE contains(z, p) RETURN z.title AS z, p.title AS p",
    CENTROID,
    "MATCH (z:Zone) MATCH (p:Parcel) WHERE contains(z, centroid(p)) RETURN p.title AS p, z.title AS z",
    "MATCH (p:Parcel) WHERE p.title <> 'parcel3' MATCH (z:Zone) WHERE contains(z, centroid(p)) "
    "RETURN p.title AS p, z.title AS z",
    "MATCH (p:Parcel) MATCH (z:Zone) WHERE contains(z, centroid(p)) AND z.title <> 'zoneC' "
    "RETURN p.title AS p, z.title AS z",
    "MATCH (p:Parcel) MATCH (z:Zone) WHERE contains(z, centroid(p)) RETURN z.title AS z, count(p) AS n",
]

# A pattern on a type that is also a second label stays off the pass (the
# existing bail) and is answered by the matcher either way.
UNFUSED = [
    "MATCH (r:Region), (t:Tag) WHERE contains(r, t) RETURN r.title AS r, t.title AS t",
]


def _norm(rows):
    return sorted(repr(sorted(row.items())) for row in rows)


def _tags(graph, query):
    return [row["operation"] for row in graph.cypher(f"EXPLAIN {query}")]


@pytest.mark.parametrize("shape", FUSED + UNFUSED)
def test_join_answers_as_the_guarded_matcher(sites, shape):
    for context in CONTEXTS:
        fused = sites.cypher(context + shape).to_list()
        plain = sites.cypher(context + shape, disabled_passes=[PASS]).to_list()
        assert _norm(fused) == _norm(plain), (context, shape)


@pytest.mark.parametrize("shape", FUSED)
def test_the_join_fuses_under_a_context(sites, shape):
    ops = _tags(sites, f"FOR VALID_TIME AS OF date('2006-01-01') {shape}")
    assert f"OptimizerPass {PASS}" in ops, (shape, ops)
    assert any(op.startswith("SpatialJoin") for op in ops), (shape, ops)


@pytest.mark.parametrize("shape", UNFUSED)
def test_a_second_label_type_keeps_the_join_unfused(sites, shape):
    ops = _tags(sites, f"FOR VALID_TIME AS OF date('2006-01-01') {shape}")
    assert f"OptimizerPass {PASS}" not in ops, (shape, ops)


def pairs(graph, instant, shape, a, b):
    prefix = "FOR VALID_TIME ALL " if instant is None else f"FOR VALID_TIME AS OF date('{instant}') "
    return sorted((r[a], r[b]) for r in graph.cypher(prefix + shape).to_list())


def test_goldens_single_match_join(sites):
    # 2002: zoneA only (zoneB starts 2005) and site1 only (site2 starts 2006).
    assert pairs(sites, "2002-06-15", SINGLE, "z", "s") == [("zoneA", "site1"), ("zoneC", "site3")]
    # 2005-06-15: both zones, site1 only: the hidden site2 at the same point
    # must not pair, and neither zone is a duplicate of the other.
    assert pairs(sites, "2005-06-15", SINGLE, "z", "s") == [
        ("zoneA", "site1"),
        ("zoneB", "site1"),
        ("zoneC", "site3"),
    ]
    # 2007-06-15: both zones, both sites.
    assert len(pairs(sites, "2007-06-15", SINGLE, "z", "s")) == 5
    # 2010-01-01: zoneA ended (half-open), site1 ended: zoneB with site2 only.
    assert pairs(sites, "2010-01-01", SINGLE, "z", "s") == [("zoneB", "site2"), ("zoneC", "site3")]
    # ALL sees every version.
    assert len(pairs(sites, None, SINGLE, "z", "s")) == 5


def test_goldens_centroid_join(sites):
    # 2002: parcel1 inside zoneA (zoneB not yet begun).
    assert pairs(sites, "2002-06-15", CENTROID, "p", "z") == [("parcel1", "zoneA"), ("parcel3", "zoneC")]
    # 2005-01-01: parcel1 ended (half-open), parcel2 not begun; zoneB begins.
    assert pairs(sites, "2005-01-01", CENTROID, "p", "z") == [("parcel3", "zoneC")]
    # 2005-06-15: parcel2 begins; both zones contain it.
    assert pairs(sites, "2005-06-15", CENTROID, "p", "z") == [
        ("parcel2", "zoneA"),
        ("parcel2", "zoneB"),
        ("parcel3", "zoneC"),
    ]
    # 2010-01-01: zoneA ended.
    assert pairs(sites, "2010-01-01", CENTROID, "p", "z") == [("parcel2", "zoneB"), ("parcel3", "zoneC")]


def test_a_context_that_hides_every_container_returns_nothing(sites):
    # Before 1990 nothing is visible at all.
    assert pairs(sites, "1980-01-01", SINGLE, "z", "s") == []
    assert pairs(sites, "1980-01-01", CENTROID, "p", "z") == []
