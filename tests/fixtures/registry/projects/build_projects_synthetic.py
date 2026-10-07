"""Write the synthetic project-ledger-shaped fixture CSVs beside this file.

Usage: python3 build_projects_synthetic.py

The shape follows a project registry's project and contract history tables,
but every company, share and date is invented (see ``../NOTICE``). The data is
built so that the two spellings of one question differ as they do on registry
tables of this shape:

* At 2009-06-30 the operator of BIRCH is ``Lead Operator AS``. Its partners
  — the other holders, at that date, of each project where it is a holder at
  that date — number 4/1/4/4/2 for ALDER/BIRCH/CEDAR/DOGWOOD/ELM.
* Every holder a project has ever had, less the operator, numbers
  18/11/18/20/19: the answer of a query that filters the operator hop by date
  and forgets the two holder hops.

Intervals are closed, as in the source tables. The output is deterministic.
"""

from __future__ import annotations

import csv
from pathlib import Path

HERE = Path(__file__).resolve().parent

LEAD, SUCCESSOR, MAJOR, LATE, FORMER_OP, STATE = 1, 2, 3, 4, 5, 6
NAMED = {
    LEAD: "Lead Operator AS",
    SUCCESSOR: "Successor Holdings AS",
    MAJOR: "Major Operator AS",
    LATE: "Late Operator ASA",
    FORMER_OP: "Former Operator AS",
    STATE: "State Partner AS",
    7: "Partner G AS",
    8: "Partner R AS",
    9: "Partner D AS",
    10: "Partner X AS",
    11: "Partner C AS",
    12: "Partner T AS",
    13: "Partner B AS",
}
FORMER_PARTNERS = list(range(101, 121))  # 20 companies holding shares before 2009

PROJECTS = {  # id: (name, current partners with their 2009 shares, Lead's share)
    43686: ("BIRCH", {STATE: 30.0}, 70.0),
    43437: ("ALDER", {7: 30.0, STATE: 20.0, MAJOR: 12.0, 8: 8.0}, 30.0),
    2762452: ("CEDAR", {9: 10.0, STATE: 36.0, MAJOR: 17.0, 10: 7.0}, 30.0),
    46437: ("DOGWOOD", {11: 1.6, STATE: 56.0, MAJOR: 8.1, 12: 3.7}, 30.6),
    3420717: ("ELM", {10: 30.0, 13: 10.0}, 60.0),
}
# Historical partners each project ever had, less the operator: 18/11/18/20/19.
EVER = {"BIRCH": 11, "ALDER": 18, "CEDAR": 18, "DOGWOOD": 20, "ELM": 19}


def operators(project_id: int, name: str) -> list[tuple]:
    if name == "CEDAR":
        return [
            (project_id, FORMER_OP, "1990-01-01", "2007-11-30"),
            (project_id, MAJOR, "2007-12-01", ""),
        ]
    rows = [
        (project_id, FORMER_OP, "1981-01-01", "2008-12-31"),
        (project_id, LEAD, "2009-01-01", "2009-10-31"),
    ]
    if name == "ELM":  # no operator in 2019
        rows += [
            (project_id, SUCCESSOR, "2009-11-01", "2018-12-31"),
            (project_id, LATE, "2020-01-01", ""),
        ]
    else:
        rows.append((project_id, SUCCESSOR, "2009-11-01", ""))
    return rows


def holders(offset: int, project_id: int, name: str) -> list[tuple]:
    partners, lead_share = PROJECTS[project_id][1], PROJECTS[project_id][2]
    rows = [(project_id, LEAD, "2009-01-01", "2009-11-01", lead_share)]
    rows += [(project_id, cmp, "2008-01-01", "", share) for cmp, share in partners.items()]
    # The successor takes Lead's share the day after Lead's row closes.
    rows.append((project_id, SUCCESSOR, "2009-11-02", "", lead_share))
    former = EVER[name] - len(partners) - 1
    for i in range(former):
        cmp = FORMER_PARTNERS[(offset * 3 + i) % len(FORMER_PARTNERS)]
        start = f"{1980 + i}-01-01"
        rows.append((project_id, cmp, start, "2007-12-31", 5.0))
    return rows


def write(name: str, header: list[str], rows: list[tuple]) -> None:
    with open(HERE / name, "w", newline="", encoding="utf-8") as fh:
        out = csv.writer(fh, lineterminator="\n")
        out.writerow(header)
        out.writerows(rows)


def main() -> None:
    companies = dict(NAMED)
    companies.update({c: f"Former Partner {c - 100:02d} AS" for c in FORMER_PARTNERS})
    write("company.csv", ["orgId", "orgLongName"], sorted(companies.items()))
    write("project.csv", ["prjId", "prjName"], [(fid, f[0]) for fid, f in PROJECTS.items()])
    ops, lics = [], []
    for offset, (fid, (name, _, _)) in enumerate(PROJECTS.items()):
        ops += operators(fid, name)
        lics += holders(offset, fid, name)
    write("project_manager_hst.csv", ["prjId", "orgId", "prjOperatorFrom", "prjOperatorTo"], ops)
    write(
        "project_holder_hst.csv",
        ["prjId", "orgId", "prjHolderFrom", "prjHolderTo", "prjCompanyShare"],
        lics,
    )
    write("contract.csv", ["ctrId", "ctrName"], [(21188, "050")])
    write(
        "contract_holder_hst.csv",
        [
            "ctrId",
            "orgId",
            "ctrHolderDateValidFrom",
            "ctrHolderDateValidTo",
            "ctrHolderInterest",
        ],
        [
            (21188, FORMER_OP, "1978-01-01", "2008-12-31", 70.0),
            (21188, LEAD, "2009-01-01", "2009-11-01", 70.0),
            (21188, STATE, "2001-01-01", "", 30.0),
            (21188, SUCCESSOR, "2009-11-02", "", 70.0),
        ],
    )


if __name__ == "__main__":
    main()
