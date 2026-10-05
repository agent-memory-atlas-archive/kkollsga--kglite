#!/usr/bin/env python3
"""Turn successive full snapshots of a source into a bitemporal history.

Demonstrates: the recording-time recipe for a source that delivers its whole
table each period and says nothing about changes. Each snapshot is compared
with the images currently on record: a new key is added, a changed key has its
current image closed and a new image added, and a vanished key is closed. The
same snapshot delivered twice changes nothing, and a snapshot older than the
latest one applied is refused.

Every value is synthetic. The bitemporal data guide
(docs/python/guides/bitemporal.md, "From successive snapshots") explains the
rules, and the outputs it shows are the ones this script prints.
"""

import hashlib
import json
import warnings

import pandas as pd

import kglite

LABEL = "Membership"
CONTENT = ["employee", "team", "role", "valid_from", "valid_to"]
DECLARED = {
    "valid_from": "validFrom",
    "valid_to": "validTo",
    "recorded_from": "datetime",
    "recorded_to": "datetime",
}


def images(snapshot, t):
    """One record per row: id <key>@<snapshot date>, a digest of the content."""
    out = snapshot.copy()
    out["key"] = out["membership"]
    out["id"] = out["membership"] + "@" + t
    out["digest"] = [
        hashlib.sha1(json.dumps([None if pd.isna(v) else str(v) for v in row]).encode()).hexdigest()[:16]
        for row in out[CONTENT].itertuples(index=False)
    ]
    out["recorded_from"] = t
    out["recorded_to"] = None
    return out.drop(columns=["membership"])


def apply_snapshot(graph, t, snapshot):
    new = images(snapshot, t)
    if LABEL not in graph.schema()["node_types"]:
        # Bootstrap: the first snapshot goes through add_nodes, which is where
        # the column types (and so the declared validity) are set.
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            graph.add_nodes(new, LABEL, "id", "id", column_types=DECLARED, convention="half_open")
        graph.cypher("MERGE (:Snapshot {id: $t})", params={"t": t})
        return {"added": len(new), "closed": 0}
    with graph.begin() as tx:
        # Read the current images under FOR VALID_TIME ALL: under the default
        # (valid today) a membership whose validity has ended is hidden, would
        # look new in every snapshot and be added again.
        current_rows = tx.cypher(
            f"FOR VALID_TIME ALL MATCH (m:{LABEL}) WHERE m.recorded_to IS NULL "
            "RETURN m.key AS key, m.id AS id, m.digest AS digest"
        ).to_list()
        # A snapshot that changed nothing leaves no image behind, so the
        # deliveries applied are recorded as markers of their own.
        latest = tx.cypher("MATCH (s:Snapshot) RETURN max(s.id) AS latest").to_list()[0]["latest"]
        if latest > t:
            raise ValueError(f"snapshot {t} is older than the latest applied ({latest})")
        tx.cypher("MERGE (:Snapshot {id: $t})", params={"t": t})
        current = {r["key"]: r for r in current_rows}
        incoming = dict(zip(new["key"], new["digest"]))
        # Changed and vanished keys both lose their current image.
        close = [r["id"] for key, r in current.items() if incoming.get(key) != r["digest"]]
        # New and changed keys both gain an image.
        add = new[[current.get(key, {}).get("digest") != digest for key, digest in incoming.items()]]
        tx.cypher(
            f"UNWIND $ids AS id MATCH (m:{LABEL} {{id: id}}) SET m.recorded_to = date($t)",
            params={"ids": close, "t": t},
        )
        rows = [{k: (None if pd.isna(v) else v) for k, v in r.items()} for r in add.to_dict("records")]
        tx.cypher(
            f"UNWIND $rows AS row MERGE (m:{LABEL} {{id: row.id}}) ON CREATE SET "
            "m.key = row.key, m.digest = row.digest, m.employee = row.employee, m.team = row.team, "
            "m.role = row.role, m.valid_from = date(row.valid_from), m.valid_to = date(row.valid_to), "
            "m.recorded_from = date($t)",
            params={"rows": rows, "t": t},
        )
    return {"added": len(rows), "closed": len(close)}


def snapshot(rows):
    return pd.DataFrame(rows, columns=["membership", *CONTENT])


# Three month-end deliveries of the whole membership table. m2's validity has
# ended (valid_to 2023-01-01); between the first and second delivery m1's
# validity ends, m2's role changes and a new membership m3 appears; by the
# third delivery m1 is gone from the source.
JAN = snapshot(
    [
        ["m1", "ada", "data", "lead", "2020-03-01", None],
        ["m2", "ben", "platform", "eng", "2022-01-01", "2023-01-01"],
    ]
)
FEB = snapshot(
    [
        ["m1", "ada", "data", "lead", "2020-03-01", "2024-02-01"],
        ["m3", "ada", "ml", "lead", "2024-02-01", None],
        ["m2", "ben", "platform", "senior", "2022-01-01", "2023-01-01"],
    ]
)
MAR = snapshot(
    [
        ["m3", "ada", "ml", "lead", "2024-02-01", None],
        ["m2", "ben", "platform", "senior", "2022-01-01", "2023-01-01"],
    ]
)

graph = kglite.KnowledgeGraph()
for t, delivery in [
    ("2024-01-31", JAN),
    ("2024-02-29", FEB),
    ("2024-02-29", FEB),  # the same delivery again
    ("2024-03-31", MAR),
    ("2024-03-31", MAR),
]:
    print(t, apply_snapshot(graph, t, delivery))
try:
    apply_snapshot(graph, "2024-02-29", FEB)
except ValueError as error:
    print("refused:", error)

# The history: every image, with the period it was on record.
for row in graph.cypher(
    f"FOR VALID_TIME ALL MATCH (m:{LABEL}) RETURN m.id AS id, m.role AS role, "
    "toString(m.recorded_from) AS recorded_from, toString(m.recorded_to) AS recorded_to ORDER BY id"
):
    print(" ", dict(row))

# What the default hides: memberships on record whose validity has ended.
ON_RECORD = f"MATCH (m:{LABEL}) WHERE m.recorded_to IS NULL RETURN m.key AS key ORDER BY key"
print("on record, valid today:", [r["key"] for r in graph.cypher(ON_RECORD)])
print("on record, all validity:", [r["key"] for r in graph.cypher("FOR VALID_TIME ALL " + ON_RECORD)])

# What the source said on 2024-02-29 about who was valid on 2024-01-15.
print(
    "known 2024-02-29, valid 2024-01-15:",
    [
        r["id"]
        for r in graph.cypher(
            f"MATCH (m:{LABEL}) WHERE m.recorded_from <= date('2024-02-29') "
            "AND (m.recorded_to IS NULL OR m.recorded_to > date('2024-02-29')) RETURN m.id AS id ORDER BY id",
            valid_at="2024-01-15",
        )
    ],
)
