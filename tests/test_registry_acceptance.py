"""Acceptance goldens for valid time on the Dutch municipal registry.

The fixture under ``tests/fixtures/registry/`` holds three open sources (see its
``NOTICE``): the RvIG municipality table (Tabel 33: code, name, successor code,
start and end, half-open — a municipality's end is its successor's start), the
RvIG count of municipalities on each 1 January 1900–2026, and the CBS
municipality → province table for 1 January 2009. Tabel 33 builds the
``Municipality`` nodes; CBS builds ``IN_PROVINCE`` memberships valid for 2009.

The expected values are hard-coded from the sources themselves, not from
KGLite: the CBS table is an independent truth for 2009, the RvIG series is a
regression pin whose known disagreement with Tabel 33 is spelled out, and the
boundary counts were cross-checked against a pandas count of Tabel 33.
"""

from __future__ import annotations

from pathlib import Path
import warnings

import pandas as pd
import pytest

import kglite

FIXTURE = Path(__file__).parent / "fixtures" / "registry"

# Tabel 33 codes that are not municipalities (unknown, foreign, RNI, ...).
PSEUDO_CODES = {"0000", "0997", "0998", "0999", "1999"}

CBS_2009 = {
    "Drenthe": 12,
    "Flevoland": 6,
    "Friesland": 31,
    "Gelderland": 56,
    "Groningen": 25,
    "Limburg": 40,
    "Noord-Brabant": 68,
    "Noord-Holland": 60,
    "Overijssel": 25,
    "Utrecht": 29,
    "Zeeland": 13,
    "Zuid-Holland": 76,
}


def _tabel33() -> pd.DataFrame:
    frame = pd.read_csv(FIXTURE / "rvig_tabel33.csv", dtype=str, keep_default_na=False)
    frame.columns = ["code", "name", "successor", "valid_from", "valid_to"]
    frame = frame[~frame["code"].isin(PSEUDO_CODES)]
    return frame.replace({"": None})


def _new_graph(storage: str, tmp_path: Path) -> kglite.KnowledgeGraph:
    if storage == "disk":
        return kglite.KnowledgeGraph(storage="disk", path=str(tmp_path / "registry"))
    if storage == "mapped":
        return kglite.KnowledgeGraph(storage="mapped")
    return kglite.KnowledgeGraph()


def build_registry(storage: str, tmp_path: Path, convention: str = "half_open") -> kglite.KnowledgeGraph:
    """Load the fixture: Tabel 33's YYYYMMDD text bounds go in as they are."""
    graph = _new_graph(storage, tmp_path)
    tabel33 = _tabel33()
    graph.add_nodes(
        tabel33[["code", "name", "valid_from", "valid_to"]],
        "Municipality",
        "code",
        "name",
        column_types={"valid_from": "validFrom", "valid_to": "validTo"},
        convention=convention,
    )
    cbs = pd.read_csv(FIXTURE / "cbs_gebieden_2009.csv", dtype=str)
    graph.add_nodes(pd.DataFrame({"province": sorted(cbs["province"].unique())}), "Province", "province", "province")
    graph.add_relationships(
        cbs.assign(valid_from="2009-01-01", valid_to="2010-01-01"),
        "IN_PROVINCE",
        "Municipality",
        "code",
        "Province",
        "province",
        column_types={"valid_from": "validFrom", "valid_to": "validTo"},
        convention="half_open",
    )
    successors = tabel33[tabel33["successor"].notna()][["code", "successor"]]
    graph.add_relationships(successors, "SUCCEEDED_BY", "Municipality", "code", "Municipality", "successor")
    return graph


@pytest.fixture(scope="module")
def registry(tmp_path_factory) -> kglite.KnowledgeGraph:
    return build_registry("memory", tmp_path_factory.mktemp("registry"))


def _national(graph: kglite.KnowledgeGraph, instant: str) -> int:
    return graph.cypher("MATCH (m:Municipality) RETURN count(*) AS n", valid_at=instant).to_list()[0]["n"]


@pytest.mark.parametrize("storage", ["memory", "mapped", "disk"])
@pytest.mark.parametrize("instant", ["2009-01-01", "2009-06-30"])
def test_cbs_2009_per_province_through_the_prefix(storage, instant, tmp_path):
    """CBS "Gebieden in Nederland 2009" lists 441 municipalities in 12
    provinces on 1 January 2009, and none changed before 1 July. Tabel 33 —
    the other source — must agree on who existed that day, so the national
    count comes from the ``Municipality`` bounds alone and the per-province
    count from the membership hop, whose nodes and relationship are both
    filtered by the one context."""
    graph = build_registry(storage, tmp_path)
    assert _national(graph, instant) == 441
    rows = graph.cypher(
        "MATCH (m:Municipality)-[:IN_PROVINCE]->(p:Province) RETURN p.title AS province, count(m) AS n",
        valid_at=instant,
    ).to_list()
    assert {r["province"]: r["n"] for r in rows} == CBS_2009


@pytest.mark.parametrize("storage", ["memory", "mapped"])
def test_cbs_2009_per_province_through_the_fluent_context(storage, tmp_path):
    """The fluent date context is the prefix's filter: the same 441 and the
    same twelve counts, walking from each province to its members."""
    graph = build_registry(storage, tmp_path)
    in_2009 = graph.date("2009-01-01")
    assert len(in_2009.select("Municipality").collect()) == 441
    counts = {
        province: len(
            in_2009.select("Province")
            .where({"title": province})
            .traverse("IN_PROVINCE", direction="incoming")
            .collect()
        )
        for province in CBS_2009
    }
    assert counts == CBS_2009


def test_cbs_2009_per_province_through_the_two_argument_functions(registry):
    """Without a context, `valid_at(x, d)` on each declared element answers the
    same — the query names every element it filters."""
    rows = registry.cypher(
        "MATCH (m:Municipality)-[r:IN_PROVINCE]->(p:Province) "
        "WHERE valid_at(m, date('2009-01-01')) AND valid_at(r, date('2009-01-01')) "
        "RETURN p.title AS province, count(m) AS n"
    ).to_list()
    assert {r["province"]: r["n"] for r in rows} == CBS_2009


def test_memberships_end_with_their_interval(registry):
    """The CBS memberships are declared for 2009 only (half-open, to
    2010-01-01), so on 2010-01-01 no province has a member through them while
    431 municipalities exist."""
    assert _national(registry, "2010-01-01") == 431
    rows = registry.cypher(
        "MATCH (m:Municipality)-[:IN_PROVINCE]->(p:Province) RETURN count(*) AS n", valid_at="2010-01-01"
    ).to_list()
    assert rows == [{"n": 0}]


def test_rvig_yearly_counts_differ_only_where_the_sources_disagree(registry):
    """RvIG publishes the number of municipalities on 1 January of every year,
    1900–2026 (127 values). Tabel 33 under its half-open convention matches
    115 of them. The other 12 — 1984 through 1995, each one too many — are the
    Zuidelijke IJsselmeerpolders public body (code 0996): Tabel 33 keeps it
    valid until 1996-01-01, while RvIG stopped counting it in 1984.

    The contract is the exact difference set, so a difference that disappears
    is as red as a new one."""
    official = pd.read_csv(FIXTURE / "rvig_aantal_gemeenten.csv")
    assert len(official) == 127
    diff = {}
    for year, expected in zip(official["year"], official["municipalities_on_jan1"]):
        got = _national(registry, f"{year}-01-01")
        if got != expected:
            diff[int(year)] = got - int(expected)
    assert diff == {year: 1 for year in range(1984, 1996)}


def test_the_boundary_day_under_each_convention(registry, tmp_path):
    """34 municipalities end on 2019-01-01 — merged into 12 successors, 9 of
    them new that day. Read half-open (the registry's meaning) 355 exist that
    day; read closed, the 34 still count and the answer is 389. A closed
    declaration of registry data warns, naming the rows that abut."""
    assert _national(registry, "2019-01-01") == 355
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        closed = build_registry("memory", tmp_path, convention="closed")
    assert any("888 of 1474 rows" in str(w.message) and "half_open" in str(w.message) for w in caught)
    assert _national(closed, "2019-01-01") == 389

    def codes(graph, instant):
        rows = graph.cypher(
            "MATCH (m:Municipality) WHERE m.id IN ['0003', '1979'] RETURN m.id AS code ORDER BY code",
            valid_at=instant,
        ).to_list()
        return [r["code"] for r in rows]

    # Appingedam (0003) ends 2021-01-01, the day Eemsdelta (1979) begins.
    assert codes(registry, "2020-12-31") == ["0003"]
    assert codes(registry, "2021-01-01") == ["1979"]
    assert codes(closed, "2021-01-01") == ["0003", "1979"]


def test_lineage_runs_without_a_context(registry):
    """A successor chain joins versions that need not coexist: Adorp (0001)
    merged into Winsum (0053) in 1990, and Winsum into Het Hogeland (1966) in
    2019. Only the unfiltered graph holds the whole chain; a context keeps the
    hops whose two ends are both valid at its instant."""
    chain = "MATCH (:Municipality {id: '0001'})-[:SUCCEEDED_BY*1..]->(x) RETURN x.id AS code ORDER BY code"
    assert [r["code"] for r in registry.cypher(chain).to_list()] == ["0053", "1966"]
    assert [r["code"] for r in registry.cypher(chain, valid_at="1989-06-30").to_list()] == ["0053"]
    assert registry.cypher(chain, valid_at="2020-01-01").to_list() == []
