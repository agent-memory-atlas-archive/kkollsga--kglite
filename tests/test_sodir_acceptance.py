"""Acceptance goldens for valid time on a SODIR-shaped licence history.

The fixture (``tests/fixtures/registry/sodir/``, synthetic — see the
``NOTICE`` beside it) has the shape of the Norwegian Offshore Directorate's
field and licence histories: five fields with operator and licensee periods
(closed intervals, as in the source tables), and production licence 050 whose
licensees use a second pair of bound properties. ``HAS_LICENSEE`` therefore
leaves two source types, each with its own declaration.

The question the fixture is built around: at 2009-06-30, which companies are
partners of the GULLFAKS operator — the other licensees, at that date, of each
field where the operator is a licensee at that date? The answer is 4/1/4/4/2
partners in GJØA/GULLFAKS/ORMEN LANGE/TROLL/VOLVE. A hand-written query that
dates the operator hop and forgets the two licensee hops answers 18/11/18/20/19
— every licensee the fields ever had — on the real tables and on this
fixture alike. Under ``FOR VALID_TIME AS OF`` no hop can be forgotten.
"""

from __future__ import annotations

from pathlib import Path

import pandas as pd
import pytest

import kglite

SODIR = Path(__file__).parent / "fixtures" / "registry" / "sodir"
T = "2009-06-30"
PARTNERS_AT_T = {"GJØA": 4, "GULLFAKS": 1, "ORMEN LANGE": 4, "TROLL": 4, "VOLVE": 2}
EVER_PARTNERS = {"GJØA": 18, "GULLFAKS": 11, "ORMEN LANGE": 18, "TROLL": 20, "VOLVE": 19}


def _read(name: str, date_columns: tuple[str, ...] = ()) -> pd.DataFrame:
    frame = pd.read_csv(SODIR / name)
    for column in date_columns:
        frame[column] = pd.to_datetime(frame[column])
    return frame


def build_sodir(storage: str) -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph(storage="mapped") if storage == "mapped" else kglite.KnowledgeGraph()
    graph.add_nodes(_read("company.csv"), "Company", "cmpNpdidCompany", "cmpLongName")
    graph.add_nodes(_read("field.csv"), "Field", "fldNpdidField", "fldName")
    graph.add_nodes(pd.read_csv(SODIR / "licence.csv", dtype={"prlName": str}), "Licence", "prlNpdidLicence", "prlName")
    graph.add_relationships(
        _read("field_operator_hst.csv", ("fldOperatorFrom", "fldOperatorTo")),
        "HAS_OPERATOR",
        "Field",
        "fldNpdidField",
        "Company",
        "cmpNpdidCompany",
        column_types={"fldOperatorFrom": "validFrom", "fldOperatorTo": "validTo"},
        convention="closed",
    )
    graph.add_relationships(
        _read("field_licensee_hst.csv", ("fldLicenseeFrom", "fldLicenseeTo")),
        "HAS_LICENSEE",
        "Field",
        "fldNpdidField",
        "Company",
        "cmpNpdidCompany",
    )
    graph.add_relationships(
        _read("licence_licensee_hst.csv", ("prlLicenseeDateValidFrom", "prlLicenseeDateValidTo")),
        "HAS_LICENSEE",
        "Licence",
        "prlNpdidLicence",
        "Company",
        "cmpNpdidCompany",
    )
    # One declaration per source type: the two tables name their bounds differently.
    graph.set_temporal("HAS_LICENSEE", "fldLicenseeFrom", "fldLicenseeTo", convention="closed", source_type="Field")
    graph.set_temporal(
        "HAS_LICENSEE",
        "prlLicenseeDateValidFrom",
        "prlLicenseeDateValidTo",
        convention="closed",
        source_type="Licence",
    )
    return graph


@pytest.fixture(scope="module", params=["memory", "mapped"])
def sodir(request) -> kglite.KnowledgeGraph:
    return build_sodir(request.param)


def _per_field(graph, query: str, **kwargs) -> dict[str, int]:
    return {r["field"]: r["partners"] for r in graph.cypher(query, **kwargs).to_list()}


NAMED = """
MATCH (:Field {title: 'GULLFAKS'})-[:HAS_OPERATOR]->(op:Company)
      <-[:HAS_LICENSEE]-(f2:Field)-[:HAS_LICENSEE]->(p:Company)
WHERE p <> op
RETURN f2.title AS field, count(DISTINCT p) AS partners
"""


def test_partners_of_the_operator_at_one_instant(sodir):
    """The prefix filters all three hops: 4/1/4/4/2."""
    assert _per_field(sodir, NAMED, valid_at=T) == PARTNERS_AT_T
    rows = sodir.cypher(
        "MATCH (:Field {title: 'GULLFAKS'})-[:HAS_OPERATOR]->(op:Company)"
        "<-[:HAS_LICENSEE]-(:Field {title: 'VOLVE'})-[:HAS_LICENSEE]->(p:Company) "
        "WHERE p <> op RETURN p.title AS partner ORDER BY partner",
        valid_at=T,
    ).to_list()
    assert [r["partner"] for r in rows] == ["Partner B AS", "Partner X AS"]


def test_the_forgotten_hop_is_impossible(sodir):
    """The fixture carries the trap: dating only the operator hop gives the
    all-time partner counts. The same text under the prefix — with or without
    its hand-written filter, with an unnamed intermediate, or with the last
    hop spelled variable-length — cannot give them."""
    forgetful = """
    MATCH (:Field {title: 'GULLFAKS'})-[o:HAS_OPERATOR]->(op:Company)
          <-[:HAS_LICENSEE]-(f2:Field)-[:HAS_LICENSEE]->(p:Company)
    WHERE valid_at(o, date($t)) AND p <> op
    RETURN f2.title AS field, count(DISTINCT p) AS partners
    """
    assert _per_field(sodir, forgetful, params={"t": T}) == EVER_PARTNERS
    assert _per_field(sodir, forgetful, params={"t": T}, valid_at=T) == PARTNERS_AT_T
    assert _per_field(sodir, NAMED, valid_at=T) == PARTNERS_AT_T
    var_length = """
    MATCH (:Field {title: 'GULLFAKS'})-[:HAS_OPERATOR]->(op:Company)
          <-[:HAS_LICENSEE]-(f2:Field)-[:HAS_LICENSEE*1..1]->(p:Company)
    WHERE p <> op
    RETURN f2.title AS field, count(DISTINCT p) AS partners
    """
    assert _per_field(sodir, var_length, valid_at=T) == PARTNERS_AT_T
    unnamed = """
    MATCH (:Field {title: 'GULLFAKS'})-[:HAS_OPERATOR]->(op:Company)
          <-[:HAS_LICENSEE]-(:Field)-[:HAS_LICENSEE]->(p:Company)
    WHERE p <> op
    RETURN count(DISTINCT p) AS partners
    """
    assert sodir.cypher(unnamed, valid_at=T).to_list() == [{"partners": 9}]


def test_the_fluent_chain_walks_the_same_hops(sodir):
    """The fluent context filters every hop too: the companies reached from
    GULLFAKS through its operator and the operator's fields are the 9 partners
    plus the operator itself."""
    reached = (
        sodir.date(T)
        .select("Field")
        .where({"title": "GULLFAKS"})
        .traverse("HAS_OPERATOR", direction="outgoing")
        .traverse("HAS_LICENSEE", direction="incoming")
        .traverse("HAS_LICENSEE", direction="outgoing")
    )
    titles = {row["title"] for row in reached.collect()}
    assert len(titles) == 10
    assert "Hydro Operator AS" in titles
    assert "Former Partner 01 AS" not in titles


def test_operator_share_and_licence_answers(sodir):
    """The operator per field, share sums of 100 and licence 050's split —
    the licence rows through their own declaration."""
    operators = sodir.cypher(
        "MATCH (f:Field)-[:HAS_OPERATOR]->(c:Company) RETURN f.title AS field, c.title AS operator ORDER BY field",
        valid_at=T,
    ).to_list()
    assert {r["field"]: r["operator"] for r in operators} == {
        "GJØA": "Hydro Operator AS",
        "GULLFAKS": "Hydro Operator AS",
        "ORMEN LANGE": "Shell-like Operator AS",
        "TROLL": "Hydro Operator AS",
        "VOLVE": "Hydro Operator AS",
    }
    shares = sodir.cypher(
        "MATCH (f:Field)-[r:HAS_LICENSEE]->() "
        "RETURN f.title AS field, round(sum(r.fldCompanyShare) * 1000) / 1000 AS total",
        valid_at=T,
    ).to_list()
    assert {r["field"]: r["total"] for r in shares} == dict.fromkeys(PARTNERS_AT_T, 100.0)
    licence = sodir.cypher(
        "MATCH (:Licence {title: '050'})-[r:HAS_LICENSEE]->(c:Company) "
        "RETURN c.title AS licensee, r.prlLicenseeInterest AS interest ORDER BY interest DESC",
        valid_at=T,
    ).to_list()
    assert licence == [
        {"licensee": "Hydro Operator AS", "interest": 70.0},
        {"licensee": "State Partner AS", "interest": 30.0},
    ]


def test_two_source_counts_match_a_pandas_oracle(sodir):
    """Each source type's rows follow their own declaration."""
    stamp = pd.Timestamp(T)

    def valid(frame, lo, hi):
        start = pd.to_datetime(frame[lo])
        end = pd.to_datetime(frame[hi])
        return int(((start <= stamp) & (end.isna() | (end >= stamp))).sum())

    expected = {
        "Field": valid(pd.read_csv(SODIR / "field_licensee_hst.csv"), "fldLicenseeFrom", "fldLicenseeTo"),
        "Licence": valid(
            pd.read_csv(SODIR / "licence_licensee_hst.csv"), "prlLicenseeDateValidFrom", "prlLicenseeDateValidTo"
        ),
    }
    assert expected == {"Field": 20, "Licence": 2}
    rows = sodir.cypher(
        "MATCH (s)-[:HAS_LICENSEE]->(:Company) RETURN labels(s)[0] AS source, count(*) AS n", valid_at=T
    ).to_list()
    assert {r["source"]: r["n"] for r in rows} == expected


@pytest.mark.parametrize(
    ("instant", "operators"),
    [("2017-06-30", ["Successor Petroleum AS"]), ("2019-06-30", []), ("2024-06-30", ["Late Operator ASA"])],
)
def test_an_operator_gap_answers_empty(sodir, instant, operators):
    """VOLVE has no operator in 2019: the OPTIONAL MATCH pads, it does not
    reach a past or a future operator."""
    rows = sodir.cypher(
        "MATCH (f:Field {title: 'VOLVE'}) OPTIONAL MATCH (f)-[:HAS_OPERATOR]->(c) RETURN collect(c.title) AS ops",
        valid_at=instant,
    ).to_list()
    assert rows == [{"ops": operators}]


def test_the_closed_boundary_day(sodir):
    """2009-11-01: the operator table hands GULLFAKS to the successor that
    day, while the licensee table keeps the old company to the end of the
    day (closed) and starts the successor on 2009-11-02."""
    day = "2009-11-01"
    ops = sodir.cypher(
        "MATCH (:Field {title: 'GULLFAKS'})-[:HAS_OPERATOR]->(c) RETURN collect(c.title) AS ops", valid_at=day
    ).to_list()
    assert ops == [{"ops": ["Successor Petroleum AS"]}]
    licensees = sodir.cypher(
        "MATCH (:Field {title: 'GULLFAKS'})-[:HAS_LICENSEE]->(c) RETURN c.title AS c ORDER BY c", valid_at=day
    ).to_list()
    assert [r["c"] for r in licensees] == ["Hydro Operator AS", "State Partner AS"]
