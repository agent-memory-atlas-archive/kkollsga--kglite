"""Reduce CBS "Gebieden in Nederland 2009" (table 72014ned) to code + province.

Usage: python3 reduce_cbs_gebieden_2009.py <72014ned_TypedDataSet.json> > cbs_gebieden_2009.csv

The input is the OData v3 ``TypedDataSet`` of the table (one record per
municipality on 1 January 2009). ``RegioS`` is the four-digit municipality
code; ``Naam_35`` is the name of the province (the ``Provincies`` group's
``Naam`` column in ``72014ned_DataProperties.json``). Rows are sorted by code;
names are stripped of the table's fixed-width padding and kept as CBS spells
them (``Friesland``).
"""

from __future__ import annotations

import csv
import json
import sys


def main(path: str) -> None:
    with open(path, encoding="utf-8") as fh:
        records = json.load(fh)
    rows = sorted((r["RegioS"].strip(), r["Naam_35"].strip()) for r in records)
    out = csv.writer(sys.stdout, lineterminator="\n")
    out.writerow(["code", "province"])
    out.writerows(rows)


if __name__ == "__main__":
    main(sys.argv[1])
