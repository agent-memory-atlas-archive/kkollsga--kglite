"""Write the synthetic SODIR-shaped fixture CSVs beside this file.

Usage: python3 build_sodir_synthetic.py

The shape follows the Norwegian Offshore Directorate's field and production
licence history tables — column names included — but every company, share and
date is invented (see ``../NOTICE``). The data is built so that the two
spellings of one question differ as they do on the real tables:

* At 2009-06-30 the operator of GULLFAKS is ``Hydro Operator AS``. Its partners
  — the other licensees, at that date, of each field where it is a licensee at
  that date — number 4/1/4/4/2 for GJØA/GULLFAKS/ORMEN LANGE/TROLL/VOLVE.
* Every licensee a field has ever had, less the operator, numbers
  18/11/18/20/19: the answer of a query that filters the operator hop by date
  and forgets the two licensee hops.

Intervals are closed, as in the source tables. The output is deterministic.
"""

from __future__ import annotations

import csv
from pathlib import Path

HERE = Path(__file__).resolve().parent

HYDRO, SUCCESSOR, SHELL, LATE, FORMER_OP, STATE = 1, 2, 3, 4, 5, 6
NAMED = {
    HYDRO: "Hydro Operator AS",
    SUCCESSOR: "Successor Petroleum AS",
    SHELL: "Shell-like Operator AS",
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

FIELDS = {  # id: (name, current partners with their 2009 shares, Hydro's share)
    43686: ("GULLFAKS", {STATE: 30.0}, 70.0),
    43437: ("GJØA", {7: 30.0, STATE: 20.0, SHELL: 12.0, 8: 8.0}, 30.0),
    2762452: ("ORMEN LANGE", {9: 10.0, STATE: 36.0, SHELL: 17.0, 10: 7.0}, 30.0),
    46437: ("TROLL", {11: 1.6, STATE: 56.0, SHELL: 8.1, 12: 3.7}, 30.6),
    3420717: ("VOLVE", {10: 30.0, 13: 10.0}, 60.0),
}
# Historical partners each field ever had, less the operator: 18/11/18/20/19.
EVER = {"GULLFAKS": 11, "GJØA": 18, "ORMEN LANGE": 18, "TROLL": 20, "VOLVE": 19}


def operators(field_id: int, name: str) -> list[tuple]:
    if name == "ORMEN LANGE":
        return [
            (field_id, FORMER_OP, "1990-01-01", "2007-11-30"),
            (field_id, SHELL, "2007-12-01", ""),
        ]
    rows = [
        (field_id, FORMER_OP, "1981-01-01", "2008-12-31"),
        (field_id, HYDRO, "2009-01-01", "2009-10-31"),
    ]
    if name == "VOLVE":  # no operator in 2019
        rows += [
            (field_id, SUCCESSOR, "2009-11-01", "2018-12-31"),
            (field_id, LATE, "2020-01-01", ""),
        ]
    else:
        rows.append((field_id, SUCCESSOR, "2009-11-01", ""))
    return rows


def licensees(offset: int, field_id: int, name: str) -> list[tuple]:
    partners, hydro_share = FIELDS[field_id][1], FIELDS[field_id][2]
    rows = [(field_id, HYDRO, "2009-01-01", "2009-11-01", hydro_share)]
    rows += [(field_id, cmp, "2008-01-01", "", share) for cmp, share in partners.items()]
    # The successor takes Hydro's share the day after Hydro's row closes.
    rows.append((field_id, SUCCESSOR, "2009-11-02", "", hydro_share))
    former = EVER[name] - len(partners) - 1
    for i in range(former):
        cmp = FORMER_PARTNERS[(offset * 3 + i) % len(FORMER_PARTNERS)]
        start = f"{1980 + i}-01-01"
        rows.append((field_id, cmp, start, "2007-12-31", 5.0))
    return rows


def write(name: str, header: list[str], rows: list[tuple]) -> None:
    with open(HERE / name, "w", newline="", encoding="utf-8") as fh:
        out = csv.writer(fh, lineterminator="\n")
        out.writerow(header)
        out.writerows(rows)


def main() -> None:
    companies = dict(NAMED)
    companies.update({c: f"Former Partner {c - 100:02d} AS" for c in FORMER_PARTNERS})
    write("company.csv", ["cmpNpdidCompany", "cmpLongName"], sorted(companies.items()))
    write("field.csv", ["fldNpdidField", "fldName"], [(fid, f[0]) for fid, f in FIELDS.items()])
    ops, lics = [], []
    for offset, (fid, (name, _, _)) in enumerate(FIELDS.items()):
        ops += operators(fid, name)
        lics += licensees(offset, fid, name)
    write("field_operator_hst.csv", ["fldNpdidField", "cmpNpdidCompany", "fldOperatorFrom", "fldOperatorTo"], ops)
    write(
        "field_licensee_hst.csv",
        ["fldNpdidField", "cmpNpdidCompany", "fldLicenseeFrom", "fldLicenseeTo", "fldCompanyShare"],
        lics,
    )
    write("licence.csv", ["prlNpdidLicence", "prlName"], [(21188, "050")])
    write(
        "licence_licensee_hst.csv",
        [
            "prlNpdidLicence",
            "cmpNpdidCompany",
            "prlLicenseeDateValidFrom",
            "prlLicenseeDateValidTo",
            "prlLicenseeInterest",
        ],
        [
            (21188, FORMER_OP, "1978-01-01", "2008-12-31", 70.0),
            (21188, HYDRO, "2009-01-01", "2009-11-01", 70.0),
            (21188, STATE, "2001-01-01", "", 30.0),
            (21188, SUCCESSOR, "2009-11-02", "", 70.0),
        ],
    )


if __name__ == "__main__":
    main()
