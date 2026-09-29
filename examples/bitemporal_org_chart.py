#!/usr/bin/env python3
"""Keep an org chart's history on two time axes: valid time and recording time.

Demonstrates: a declared half-open validity interval (native), a hand-written
recording pair (modelled), superseded images kept as their own records, a daily
HR change feed applied in one transaction (with the "was" check, a no-op
redelivery and a rollback), and the as-of, as-known-at, both-axes, lineage,
changed-since and audit questions.

Every value is synthetic. The bitemporal data guide
(docs/python/guides/bitemporal.md) walks through the same steps, and the
outputs it shows are the ones this script prints.
"""

import warnings

import pandas as pd

import kglite

# -- 1. Set-up: anchors, assignment records, department links -----------------

# One record per assignment image. The id is <employee>.<assignment>:<image>,
# where "r" is the image as recorded and "e" the image as ended. ben.1 was
# created and cancelled on the same day, so its interval is empty.
assignments = pd.DataFrame(
    {
        "id": ["ada.1:r", "ben.1:r", "ben.2:r"],
        "employee": ["ada", "ben", "ben"],
        "team": ["data", "data", "platform"],
        "valid_from": ["2020-03-01", "2022-01-01", "2022-01-01"],
        "valid_to": [None, "2022-01-01", None],
        "recorded_from": ["2020-03-02", "2022-01-03", "2022-01-03"],
        "recorded_to": [None, None, None],
    }
)
part_of = pd.DataFrame(
    {
        "team": ["data", "platform"],
        "department": ["product", "eng"],
        "valid_from": ["2018-01-01", "2022-01-01"],
        "valid_to": [None, None],
        "recorded_from": ["2018-01-02", "2022-01-03"],
        "recorded_to": [None, None],
    }
)
DECLARED = {
    "valid_from": "validFrom",
    "valid_to": "validTo",
    "recorded_from": "datetime",
    "recorded_to": "datetime",
}

graph = kglite.KnowledgeGraph()
graph.add_nodes(pd.DataFrame({"id": ["ada", "ben"], "name": ["Ada", "Ben"]}), "Employee", "id", "name")
graph.add_nodes(pd.DataFrame({"id": ["data", "platform"], "name": ["Data", "Platform"]}), "Team", "id", "name")
graph.add_nodes(
    pd.DataFrame({"id": ["eng", "product"], "name": ["Engineering", "Product"]}), "Department", "id", "name"
)
# The empty assignment ben.1:r is kept, with one warning.
with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    graph.add_nodes(assignments, "Assignment", "id", "id", column_types=DECLARED, convention="half_open")
for warning in caught:
    print("load warning:", warning.message)
graph.add_relationships(assignments, "OF", "Assignment", "id", "Employee", "employee")
graph.add_relationships(assignments, "TO", "Assignment", "id", "Team", "team")
graph.add_relationships(
    part_of, "PART_OF", "Team", "team", "Department", "department", column_types=DECLARED, convention="half_open"
)
redelivered = graph.add_relationships(
    part_of, "PART_OF", "Team", "team", "Department", "department", column_types=DECLARED, convention="half_open"
)
print("redelivered links:", {k: redelivered[k] for k in ("connections_created", "connections_updated")})
print(
    "valid on 2022-01-01:",
    graph.cypher("MATCH (a:Assignment {employee: 'ben'}) RETURN a.id AS id", valid_at="2022-01-01").to_list(),
)
print(
    "declarations:",
    graph.cypher("""
        CALL db.temporal.declarations() YIELD kind, name, convention, abutting_rows, empty_rows
        RETURN kind, name, convention, abutting_rows, empty_rows
    """).to_list(),
)


# -- 2. A day's delivery, applied in one transaction --------------------------


def apply_delivery(graph, t, supersedes, new_links, new_assignments):
    with graph.begin() as tx:
        # The "was" check: every image the delivery supersedes is current,
        # or was superseded by this same delivery (a redelivery).
        found = tx.cypher(
            """
            MATCH (a:Assignment) WHERE a.id IN $ids
              AND (a.recorded_to IS NULL OR a.recorded_to = date($t))
            RETURN count(a) AS n
            """,
            params={"ids": supersedes, "t": t},
        ).to_list()[0]["n"]
        if found != len(supersedes):
            raise ValueError(f"delivery {t}: {found} of {len(supersedes)} superseded records are current")
        # Close the recording period of the superseded images.
        tx.cypher(
            """
            UNWIND $ids AS id
            MATCH (a:Assignment {id: id}) WHERE a.recorded_to IS NULL
            SET a.recorded_to = date($t)
            """,
            params={"ids": supersedes, "t": t},
        )
        # New teams and their department links come first, so assignments can reach them.
        tx.cypher(
            """
            UNWIND $rows AS row
            MATCH (d:Department {id: row.department})
            MERGE (m:Team {id: row.team}) ON CREATE SET m.title = row.name
            MERGE (m)-[:PART_OF {valid_from: date(row.valid_from), recorded_from: date($t)}]->(d)
            """,
            params={"rows": new_links, "t": t},
        )
        # Add the new images, each under its own id, linked to its anchors.
        tx.cypher(
            """
            UNWIND $rows AS row
            MATCH (e:Employee {id: row.employee}), (m:Team {id: row.team})
            MERGE (a:Assignment {id: row.id})
              ON CREATE SET a.employee = row.employee, a.team = row.team,
                            a.valid_from = date(row.valid_from), a.valid_to = date(row.valid_to),
                            a.recorded_from = date($t)
            MERGE (a)-[:OF]->(e)
            MERGE (a)-[:TO]->(m)
            """,
            params={"rows": new_assignments, "t": t},
        )


# On 2024-06-15 HR records that Ada moved from Data to a new Machine Learning
# team on 2024-06-01.
delivery = dict(
    t="2024-06-15",
    supersedes=["ada.1:r"],
    new_links=[{"team": "ml", "name": "Machine Learning", "department": "eng", "valid_from": "2024-06-01"}],
    new_assignments=[
        {"id": "ada.1:e", "employee": "ada", "team": "data", "valid_from": "2020-03-01", "valid_to": "2024-06-01"},
        {"id": "ada.2:r", "employee": "ada", "team": "ml", "valid_from": "2024-06-01", "valid_to": None},
    ],
)
COUNTS = "MATCH (n) RETURN count(n) AS nodes, COUNT { ()-[]->() } AS relationships"
apply_delivery(graph, **delivery)
print("after the delivery:", graph.cypher(COUNTS).to_list())
apply_delivery(graph, **delivery)  # the same day, delivered again
print("after a redelivery:", graph.cypher(COUNTS).to_list())
try:
    apply_delivery(graph, "2024-06-20", ["ada.1:r"], [], [])
except ValueError as error:
    print("refused:", error)
try:
    apply_delivery(
        graph,
        "2024-06-20",
        ["ben.2:r"],
        [],
        [
            {
                "id": "ben.2:e",
                "employee": "ben",
                "team": "platform",
                "valid_from": "2022-01-01",
                "valid_to": "2021-12-31",
            }
        ],
    )
except kglite.CypherExecutionError as error:
    print("rolled back:", error)
print(
    "ben.2:r after the rollback:",
    graph.cypher("MATCH (a:Assignment {id: 'ben.2:r'}) RETURN a.recorded_to AS recorded_to").to_list(),
)

# -- 3. Questions --------------------------------------------------------------

CURRENT = """
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)
    WHERE a.recorded_to IS NULL
    RETURN e.title AS employee, t.title AS team ORDER BY employee
"""
for day in ["2024-05-01", "2024-07-01"]:
    print(f"as of {day}, as known now:", graph.cypher(CURRENT, valid_at=day).to_list())
print(
    "fluent, as of 2024-07-01:",
    sorted(graph.date("2024-07-01").select("Assignment").where({"recorded_to": {"is_null": True}}).ids()),
)

# The recording test, as one template applied to every hop.
KNOWN = "{x}.recorded_from <= date($tt) AND ({x}.recorded_to IS NULL OR {x}.recorded_to > date($tt))"
AS_KNOWN = f"""
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)
    WHERE {KNOWN.format(x="a")}
    RETURN e.title AS employee, t.title AS team ORDER BY employee
"""
for tt in ["2024-06-14", "2024-06-15"]:
    print(
        f"as of 2024-07-01, as known on {tt}:",
        graph.cypher(AS_KNOWN, params={"tt": tt}, valid_at="2024-07-01").to_list(),
    )
print(
    "as of 2021-06-30, as known on 2021-06-30:",
    graph.cypher(AS_KNOWN, params={"tt": "2021-06-30"}, valid_at="2021-06-30").to_list(),
)

BOTH_HOPS = f"""
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)-[p:PART_OF]->(d:Department)
    WHERE {KNOWN.format(x="a")} AND {KNOWN.format(x="p")}
    RETURN e.title AS employee, t.title AS team, d.title AS department ORDER BY employee
"""
for tt in ["2024-06-14", "2024-06-15"]:
    print(f"both axes, known on {tt}:", graph.cypher(BOTH_HOPS, params={"tt": tt}, valid_at="2024-07-01").to_list())

LINEAGE = f"""
    MATCH (a:Assignment {{employee: $employee}})
    WHERE {KNOWN.format(x="a")}
    RETURN a.team AS team, toString(a.valid_from) AS valid_from, toString(a.valid_to) AS valid_to
    ORDER BY valid_from, valid_to
"""
for employee, tt in [("ada", "2024-06-14"), ("ada", "2024-06-15"), ("ben", "2024-06-15")]:
    print(
        f"lineage of {employee}, known on {tt}:",
        graph.cypher(LINEAGE, params={"employee": employee, "tt": tt}).to_list(),
    )

print(
    "changed since 2024-06-01:",
    graph.cypher(
        """
        MATCH (a:Assignment)
        WHERE a.recorded_from > date($since) OR a.recorded_to > date($since)
        RETURN DISTINCT a.employee AS employee
        """,
        params={"since": "2024-06-01"},
    ).to_list(),
)
print(
    "delivery of 2024-06-15:",
    graph.cypher(
        """
        MATCH (a:Assignment)
        WHERE a.recorded_from = date($day) OR a.recorded_to = date($day)
        RETURN a.id AS record,
               CASE WHEN a.recorded_to = date($day) THEN 'superseded' ELSE 'added' END AS change
        ORDER BY record
        """,
        params={"day": "2024-06-15"},
    ).to_list(),
)
print(
    "recorded late:",
    graph.cypher("""
        MATCH (a:Assignment)
        WHERE a.recorded_from > coalesce(a.valid_to, a.valid_from) + duration({days: 7})
        RETURN a.id AS record, toString(coalesce(a.valid_to, a.valid_from)) AS effective,
               toString(a.recorded_from) AS recorded
        ORDER BY record
    """).to_list(),
)

# The named form reads the recording pair as closed, so on the recording day it
# matches both the image that ended and the one that began.
print(
    "named form, known on 2024-06-15:",
    graph.cypher(
        """
        MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)
        WHERE valid_at(a, $tt, 'recorded_from', 'recorded_to')
        RETURN e.title AS employee, t.title AS team ORDER BY employee, team
        """,
        params={"tt": "2024-06-15"},
        valid_at="2024-07-01",
    ).to_list(),
)
# Leaving the recording test off one hop reads every image on that hop.
ONE_HOP = f"""
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)-[p:PART_OF]->(d:Department)
    WHERE {KNOWN.format(x="p")}
    RETURN e.title AS employee, t.title AS team, d.title AS department ORDER BY employee, team
"""
print("one hop tested:", graph.cypher(ONE_HOP, params={"tt": "2024-06-15"}, valid_at="2024-07-01").to_list())

# Audits on the current images: containment in the team's department period,
# and overlapping assignments of one employee.
print(
    "outside the team's department period:",
    graph.cypher("""
        MATCH (a:Assignment)-[:TO]->(:Team)-[p:PART_OF]->(:Department)
        WHERE a.recorded_to IS NULL AND p.recorded_to IS NULL
          AND ((p.valid_from IS NOT NULL AND (a.valid_from IS NULL OR a.valid_from < p.valid_from))
            OR (p.valid_to IS NOT NULL AND (a.valid_to IS NULL OR a.valid_to > p.valid_to)))
        RETURN a.id AS record
    """).to_list(),
)
print(
    "overlapping assignments:",
    graph.cypher("""
        MATCH (e:Employee)<-[:OF]-(a:Assignment), (e)<-[:OF]-(b:Assignment)
        WHERE a.id < b.id AND a.recorded_to IS NULL AND b.recorded_to IS NULL
          AND coalesce(a.valid_from, date('0001-01-01')) < coalesce(b.valid_to, date('9999-12-31'))
          AND coalesce(b.valid_from, date('0001-01-01')) < coalesce(a.valid_to, date('9999-12-31'))
        RETURN e.id AS employee, a.id AS first, b.id AS second
    """).to_list(),
)

# The echo names the valid-time context; the recording test is plain WHERE text.
print("echo:", graph.cypher(AS_KNOWN, params={"tt": "2024-06-15"}, valid_at="2024-07-01").diagnostics["temporal"])
