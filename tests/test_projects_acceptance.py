"""Acceptance goldens for valid time on a project-ledger-shaped contract history.

The fixture (``tests/fixtures/registry/projects/``, synthetic — see the
``NOTICE`` beside it) has the shape of a project registry's
project and contract histories: five projects with operator and holder periods
(closed intervals, as in the source tables), and contract 050 whose
holders use a second pair of bound properties. ``HAS_HOLDER`` therefore
leaves two source types, each with its own declaration.

The question the fixture is built around: at 2009-06-30, which companies are
partners of the BIRCH operator — the other holders, at that date, of each
project where the operator is a holder at that date? The answer is 4/1/4/4/2
partners in ALDER/BIRCH/CEDAR/DOGWOOD/ELM. A hand-written query that
dates the operator hop and forgets the two holder hops answers 18/11/18/20/19
— every holder the projects ever had — on tables of this shape and on this
fixture alike. Under ``FOR VALID_TIME AS OF`` no hop can be forgotten.
"""

from __future__ import annotations

from pathlib import Path

import pandas as pd
import pytest

import kglite

FIXTURE_DIR = Path(__file__).parent / "fixtures" / "registry" / "projects"
T = "2009-06-30"
PARTNERS_AT_T = {"ALDER": 4, "BIRCH": 1, "CEDAR": 4, "DOGWOOD": 4, "ELM": 2}
EVER_PARTNERS = {"ALDER": 18, "BIRCH": 11, "CEDAR": 18, "DOGWOOD": 20, "ELM": 19}


def _read(name: str, date_columns: tuple[str, ...] = ()) -> pd.DataFrame:
    frame = pd.read_csv(FIXTURE_DIR / name)
    for column in date_columns:
        frame[column] = pd.to_datetime(frame[column])
    return frame


def build_projects(storage: str) -> kglite.KnowledgeGraph:
    graph = kglite.KnowledgeGraph(storage="mapped") if storage == "mapped" else kglite.KnowledgeGraph()
    graph.add_nodes(_read("company.csv"), "Company", "orgId", "orgLongName")
    graph.add_nodes(_read("project.csv"), "Project", "prjId", "prjName")
    graph.add_nodes(pd.read_csv(FIXTURE_DIR / "contract.csv", dtype={"ctrName": str}), "Contract", "ctrId", "ctrName")
    graph.add_relationships(
        _read("project_manager_hst.csv", ("prjOperatorFrom", "prjOperatorTo")),
        "MANAGED_BY",
        "Project",
        "prjId",
        "Company",
        "orgId",
        column_types={"prjOperatorFrom": "validFrom", "prjOperatorTo": "validTo"},
        convention="closed",
    )
    graph.add_relationships(
        _read("project_holder_hst.csv", ("prjHolderFrom", "prjHolderTo")),
        "HAS_HOLDER",
        "Project",
        "prjId",
        "Company",
        "orgId",
    )
    graph.add_relationships(
        _read("contract_holder_hst.csv", ("ctrHolderDateValidFrom", "ctrHolderDateValidTo")),
        "HAS_HOLDER",
        "Contract",
        "ctrId",
        "Company",
        "orgId",
    )
    # One declaration per source type: the two tables name their bounds differently.
    graph.set_temporal("HAS_HOLDER", "prjHolderFrom", "prjHolderTo", convention="closed", source_type="Project")
    graph.set_temporal(
        "HAS_HOLDER",
        "ctrHolderDateValidFrom",
        "ctrHolderDateValidTo",
        convention="closed",
        source_type="Contract",
    )
    return graph


@pytest.fixture(scope="module", params=["memory", "mapped"])
def ledger(request) -> kglite.KnowledgeGraph:
    return build_projects(request.param)


def _per_project(graph, query: str, **kwargs) -> dict[str, int]:
    return {r["project"]: r["partners"] for r in graph.cypher(query, **kwargs).to_list()}


NAMED = """
MATCH (:Project {title: 'BIRCH'})-[:MANAGED_BY]->(op:Company)
      <-[:HAS_HOLDER]-(f2:Project)-[:HAS_HOLDER]->(p:Company)
WHERE p <> op
RETURN f2.title AS project, count(DISTINCT p) AS partners
"""


def test_partners_of_the_operator_at_one_instant(ledger):
    """The prefix filters all three hops: 4/1/4/4/2."""
    assert _per_project(ledger, NAMED, valid_at=T) == PARTNERS_AT_T
    rows = ledger.cypher(
        "MATCH (:Project {title: 'BIRCH'})-[:MANAGED_BY]->(op:Company)"
        "<-[:HAS_HOLDER]-(:Project {title: 'ELM'})-[:HAS_HOLDER]->(p:Company) "
        "WHERE p <> op RETURN p.title AS partner ORDER BY partner",
        valid_at=T,
    ).to_list()
    assert [r["partner"] for r in rows] == ["Partner B AS", "Partner X AS"]


def test_the_forgotten_hop_is_impossible(ledger):
    """The fixture carries the trap: dating only the operator hop gives the
    all-time partner counts. The same text under the prefix — with or without
    its hand-written filter, with an unnamed intermediate, or with the last
    hop spelled variable-length — cannot give them."""
    forgetful = """
    MATCH (:Project {title: 'BIRCH'})-[o:MANAGED_BY]->(op:Company)
          <-[:HAS_HOLDER]-(f2:Project)-[:HAS_HOLDER]->(p:Company)
    WHERE valid_at(o, date($t)) AND p <> op
    RETURN f2.title AS project, count(DISTINCT p) AS partners
    """
    assert _per_project(ledger, forgetful, params={"t": T}) == EVER_PARTNERS
    assert _per_project(ledger, forgetful, params={"t": T}, valid_at=T) == PARTNERS_AT_T
    assert _per_project(ledger, NAMED, valid_at=T) == PARTNERS_AT_T
    var_length = """
    MATCH (:Project {title: 'BIRCH'})-[:MANAGED_BY]->(op:Company)
          <-[:HAS_HOLDER]-(f2:Project)-[:HAS_HOLDER*1..1]->(p:Company)
    WHERE p <> op
    RETURN f2.title AS project, count(DISTINCT p) AS partners
    """
    assert _per_project(ledger, var_length, valid_at=T) == PARTNERS_AT_T
    unnamed = """
    MATCH (:Project {title: 'BIRCH'})-[:MANAGED_BY]->(op:Company)
          <-[:HAS_HOLDER]-(:Project)-[:HAS_HOLDER]->(p:Company)
    WHERE p <> op
    RETURN count(DISTINCT p) AS partners
    """
    assert ledger.cypher(unnamed, valid_at=T).to_list() == [{"partners": 9}]


def test_the_fluent_chain_walks_the_same_hops(ledger):
    """The fluent context filters every hop too: the companies reached from
    BIRCH through its operator and the operator's projects are the 9 partners
    plus the operator itself."""
    reached = (
        ledger.date(T)
        .select("Project")
        .where({"title": "BIRCH"})
        .traverse("MANAGED_BY", direction="outgoing")
        .traverse("HAS_HOLDER", direction="incoming")
        .traverse("HAS_HOLDER", direction="outgoing")
    )
    titles = {row["title"] for row in reached.collect()}
    assert len(titles) == 10
    assert "Lead Operator AS" in titles
    assert "Former Partner 01 AS" not in titles


def test_operator_share_and_contract_answers(ledger):
    """The operator per project, share sums of 100 and contract 050's split —
    the contract rows through their own declaration."""
    operators = ledger.cypher(
        "MATCH (f:Project)-[:MANAGED_BY]->(c:Company) RETURN f.title AS project, c.title AS operator ORDER BY project",
        valid_at=T,
    ).to_list()
    assert {r["project"]: r["operator"] for r in operators} == {
        "ALDER": "Lead Operator AS",
        "BIRCH": "Lead Operator AS",
        "CEDAR": "Major Operator AS",
        "DOGWOOD": "Lead Operator AS",
        "ELM": "Lead Operator AS",
    }
    shares = ledger.cypher(
        "MATCH (f:Project)-[r:HAS_HOLDER]->() "
        "RETURN f.title AS project, round(sum(r.prjCompanyShare) * 1000) / 1000 AS total",
        valid_at=T,
    ).to_list()
    assert {r["project"]: r["total"] for r in shares} == dict.fromkeys(PARTNERS_AT_T, 100.0)
    contract = ledger.cypher(
        "MATCH (:Contract {title: '050'})-[r:HAS_HOLDER]->(c:Company) "
        "RETURN c.title AS holder, r.ctrHolderInterest AS interest ORDER BY interest DESC",
        valid_at=T,
    ).to_list()
    assert contract == [
        {"holder": "Lead Operator AS", "interest": 70.0},
        {"holder": "State Partner AS", "interest": 30.0},
    ]


def test_two_source_counts_match_a_pandas_oracle(ledger):
    """Each source type's rows follow their own declaration."""
    stamp = pd.Timestamp(T)

    def valid(frame, lo, hi):
        start = pd.to_datetime(frame[lo])
        end = pd.to_datetime(frame[hi])
        return int(((start <= stamp) & (end.isna() | (end >= stamp))).sum())

    expected = {
        "Project": valid(pd.read_csv(FIXTURE_DIR / "project_holder_hst.csv"), "prjHolderFrom", "prjHolderTo"),
        "Contract": valid(
            pd.read_csv(FIXTURE_DIR / "contract_holder_hst.csv"), "ctrHolderDateValidFrom", "ctrHolderDateValidTo"
        ),
    }
    assert expected == {"Project": 20, "Contract": 2}
    rows = ledger.cypher(
        "MATCH (s)-[:HAS_HOLDER]->(:Company) RETURN labels(s)[0] AS source, count(*) AS n", valid_at=T
    ).to_list()
    assert {r["source"]: r["n"] for r in rows} == expected


@pytest.mark.parametrize(
    ("instant", "operators"),
    [("2017-06-30", ["Successor Holdings AS"]), ("2019-06-30", []), ("2024-06-30", ["Late Operator ASA"])],
)
def test_an_operator_gap_answers_empty(ledger, instant, operators):
    """ELM has no operator in 2019: the OPTIONAL MATCH pads, it does not
    reach a past or a future operator."""
    rows = ledger.cypher(
        "MATCH (f:Project {title: 'ELM'}) OPTIONAL MATCH (f)-[:MANAGED_BY]->(c) RETURN collect(c.title) AS ops",
        valid_at=instant,
    ).to_list()
    assert rows == [{"ops": operators}]


def test_the_closed_boundary_day(ledger):
    """2009-11-01: the operator table hands BIRCH to the successor that
    day, while the holder table keeps the old company to the end of the
    day (closed) and starts the successor on 2009-11-02."""
    day = "2009-11-01"
    ops = ledger.cypher(
        "MATCH (:Project {title: 'BIRCH'})-[:MANAGED_BY]->(c) RETURN collect(c.title) AS ops", valid_at=day
    ).to_list()
    assert ops == [{"ops": ["Successor Holdings AS"]}]
    holders = ledger.cypher(
        "MATCH (:Project {title: 'BIRCH'})-[:HAS_HOLDER]->(c) RETURN c.title AS c ORDER BY c", valid_at=day
    ).to_list()
    assert [r["c"] for r in holders] == ["Lead Operator AS", "State Partner AS"]
