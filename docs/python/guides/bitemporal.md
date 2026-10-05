# Bitemporal data

A history that is corrected after the fact answers two questions about every
fact: when was it true in the world, and when did the system know it.

An HR system learns of a transfer late and backdates it. Payroll and last
week's org-chart export saw the old answer, and an audit needs to reproduce
exactly that.

This page is for the architect of such a store, with a Java or Python ingest,
who knows valid time and transaction time already. It answers three questions:

- what KGLite gives natively;
- what you model yourself;
- what it costs.

The examples build one small synthetic org chart: two employees, their team
assignments, and the departments the teams belong to. The code blocks run in
order as one script. The same steps, and every output shown here, come from
the runnable
[`examples/bitemporal_org_chart.py`](https://github.com/kkollsga/kglite/blob/main/examples/bitemporal_org_chart.py).

Valid time itself is covered in depth in {doc}`valid-time`, which this page
builds on. That page covers its declarations, conventions and fluent API.

## 1. Two axes: one native, one modelled

**Valid time is native.** A node label or relationship type declares the two
properties that bound its validity interval. A statement asked
`FOR VALID_TIME AS OF` an instant (or with `valid_at=`) then answers as if the
graph held only the elements valid then. That holds on every hop, path,
subquery and algorithm, and every binding writes the same prefix. A statement
with no prefix is asked as of today. `FOR VALID_TIME ALL` reads every version.

**Recording time is modelled.** KGLite keeps no recording time of its own. When
your source (or your ingest) knew a fact is a second pair of bounds, such as
`recorded_from` / `recorded_to`. You store that pair on records you never
overwrite, and test it by hand in the query. The engine checks nothing about
that pair.

The rest of this page is about doing that well.

## 2. The feature surface

| Capability | Status | How |
|---|---|---|
| Valid-time declaration, closed or half-open | native | `validFrom` / `validTo`, `set_temporal()`, `db.temporal.declare`, blueprint `temporal` |
| As of a valid instant, every hop, path, subquery and algorithm | native | `FOR VALID_TIME AS OF`, `valid_at=`, `freeze(valid_at=)`, fluent `date()` |
| Write-time validation of valid-time bounds | native | loads and Cypher writes onto a declared type refuse an inverted interval |
| Redelivery of a declared relationship type | native | identical rows dropped, differing rows become versions |
| Atomic delivery | native | `begin()` (Python), `beginTransaction()` (Java); a failure rolls the whole transaction back |
| Valid-time echo | native | `diagnostics["temporal"]`, MCP `temporal:`, Bolt `kglite.temporal` |
| Recording ("as known at") time | modelled | a second pair of properties, tested half-open by hand on every hop |
| Superseded images | modelled | one record per image, each with its own id |
| Zero-length versions | native | kept with a warning and counted in `empty_rows`; valid at no instant, so only queries that read every version (`FOR VALID_TIME ALL`: lineage, audit) see them |
| Changed since, delivery replay | modelled | queries on the recording pair |
| Reproducing an earlier answer | modelled | the recording pair, or keeping each published `.kgl` |
| Engine transaction time / audit trail | not there | `SET` overwrites and `DELETE` leaves no trace; CDC is process-local |
| `ASSERT` in a transaction | not there | raise from Python, or an integer division by zero in the statement |
| As-of on the graph-wide Python algorithm methods | not there | `pagerank()` and the other graph-wide methods ignore `date()`; run `CALL pagerank()` under `valid_at=` |
| Many valid instants in one statement | native for a node label | `UNWIND` of instants with `valid_at(x, d)` over one labelled pattern scans it once; other shapes materialise instants × rows, so run one statement per instant |

## 3. Set-up

### Declare valid time

Declare valid time on every type that carries it, in any of the four ways
{doc}`valid-time` describes.

**Choose the convention from the source.** An HR change feed is half-open: a
transfer ends the old assignment on the day the new one begins. Declare it
`half_open`, so the transfer day belongs to the new assignment alone.

Declared `closed`, the same rows:

- count both sides of every boundary day;
- put the employee on two teams on the day of each transfer;
- make the load warn that rows end on the day another begins.

### Model recording time

Give each record the pair `recorded_from` / `recorded_to` and follow three
rules.

**Rule 1: keep every superseded image as its own record.** When HR records a
change, it also closes the previous assignment. Until then the system knew that
assignment as open-ended. From then on it knows it as closed.

- Keep both images: the open-ended one, recorded until the change was recorded,
  and the closed one, recorded from then on.
- The record count is therefore higher than the assignment count.
- Do not copy the recording date of the change onto the old image as its
  `valid_to`. The assignment ended on the transfer date, not on the day HR typed
  it in.

**Rule 2: give each record its own id, and keep the entity key in a separate,
non-unique property.** The example names a record `<employee>.<assignment>:<r|e>`,
for the image *as recorded* and *as ended*.

Records that share an id shadow each other:

- `MATCH (a {id: …})` finds one node per id, and can land on an image recorded
  at another time.
- A node load onto a stored id updates that node instead of adding one.
- The only hint is a duplicate-id warning in the writing statement's
  `result.warnings`.

**Rule 3: link records to timeless anchors.** Employees, teams and departments
are anchor nodes. Each assignment record links to its employee (`OF`) and its
team (`TO`), so a new record never rewrites the anchors or the links between
them.

Load the recording columns with the first load, even when every value is still
NULL. A column a loader writes is a known property of the type from then on. A
property that no write has named yet is refused by a later `CREATE` as a
probable typo.

### Load the extract

The extract holds three assignment records and two department links. `ben.1` is
a zero-length assignment: HR put Ben on the Data team and moved him to Platform
the same day, so its `valid_from` equals its `valid_to`:

```python
import warnings

import kglite
import pandas as pd

assignments = pd.DataFrame({
    "id":            ["ada.1:r",    "ben.1:r",    "ben.2:r"],
    "employee":      ["ada",        "ben",        "ben"],
    "team":          ["data",       "data",       "platform"],
    "valid_from":    ["2020-03-01", "2022-01-01", "2022-01-01"],
    "valid_to":      [None,         "2022-01-01", None],
    "recorded_from": ["2020-03-02", "2022-01-03", "2022-01-03"],
    "recorded_to":   [None,         None,         None],
})
part_of = pd.DataFrame({
    "team": ["data", "platform"], "department": ["product", "eng"],
    "valid_from": ["2018-01-01", "2022-01-01"], "valid_to": [None, None],
    "recorded_from": ["2018-01-02", "2022-01-03"], "recorded_to": [None, None],
})
DECLARED = {"valid_from": "validFrom", "valid_to": "validTo",
            "recorded_from": "datetime", "recorded_to": "datetime"}

graph = kglite.KnowledgeGraph()
graph.add_nodes(pd.DataFrame({"id": ["ada", "ben"], "name": ["Ada", "Ben"]}), "Employee", "id", "name")
graph.add_nodes(pd.DataFrame({"id": ["data", "platform"], "name": ["Data", "Platform"]}), "Team", "id", "name")
graph.add_nodes(pd.DataFrame({"id": ["eng", "product"], "name": ["Engineering", "Product"]}),
                "Department", "id", "name")
with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    graph.add_nodes(assignments, "Assignment", "id", "id", column_types=DECLARED, convention="half_open")
[str(w.message) for w in caught]
# ["1 of 3 rows written have an empty interval under convention 'half_open' (the
# from bound equals the to bound) and are valid at no instant; the first is row
# 1 (0-based) of the load. They are stored and counted in
# db.temporal.declarations() as empty_rows."]
graph.add_relationships(assignments, "OF", "Assignment", "id", "Employee", "employee")
graph.add_relationships(assignments, "TO", "Assignment", "id", "Team", "team")
graph.add_relationships(part_of, "PART_OF", "Team", "team", "Department", "department",
                        column_types=DECLARED, convention="half_open")
```

### Zero-length versions

**A zero-length version is valid at no instant under `half_open`.** Such rows
are legitimate (an assignment created and cancelled on the same day). The load
therefore keeps them, warns once with their count and the first of them, and
`db.temporal.declarations()` counts them in `empty_rows`.

- Every later load or Cypher `CREATE`, `MERGE` or `SET` onto the declared type
  does the same.
- An inverted interval, whose `valid_to` precedes its `valid_from`, is refused.
- No as-of question returns a zero-length version.

On the day of `ben.1`, only its successor is valid:

```python
graph.cypher("MATCH (a:Assignment {employee: 'ben'}) RETURN a.id AS id",
             valid_at="2022-01-01").to_list()
# [{'id': 'ben.2:r'}]
```

Lineage and audit queries, which read every version under
`FOR VALID_TIME ALL`, still read it (section 5).

### Versions on a declared relationship type

**On a declared relationship type, a load writes versions.** `add_relationships`
(and `replace_relationships`, `create_relationships()`, `extend()` and
blueprints) never updates a stored relationship.

- A row identical to one already between its endpoints is dropped, so a
  redelivered file is harmless.
- A row that differs in anything becomes a new, parallel relationship. That
  includes a different `valid_to`, `recorded_from` or other property.
- `connections_updated` stays 0.

```python
redelivered = graph.add_relationships(part_of, "PART_OF", "Team", "team", "Department", "department",
                                      column_types=DECLARED, convention="half_open")
{k: redelivered[k] for k in ("connections_created", "connections_updated")}
# {'connections_created': 0, 'connections_updated': 0}
```

Close or correct a stored relationship period with Cypher `SET` when that is
what you mean.

`db.temporal.declarations()` lists what is declared:

- `abutting_rows` counts the rows whose `valid_to` is another row's
  `valid_from` within one entity. That is a transfer chain showing through. Here
  it shows only a relationship's, since every `Assignment` has its own id.
- `empty_rows` counts the rows valid at no instant. Here that is the
  zero-length assignment.

```python
graph.cypher("""
    CALL db.temporal.declarations() YIELD kind, name, convention, abutting_rows, empty_rows
    RETURN kind, name, convention, abutting_rows, empty_rows
""").to_list()
# [{'kind': 'node', 'name': 'Assignment', 'convention': 'half_open', 'abutting_rows': 0, 'empty_rows': 1},
#  {'kind': 'relationship', 'name': 'PART_OF', 'convention': 'half_open', 'abutting_rows': 0, 'empty_rows': 0}]
```

## 4. Apply a day's delivery

HR's daily change feed closes the recording period of the images it supersedes
and adds the new ones. Apply it in one transaction. No reader then sees the day
half applied, and a failure anywhere rolls the whole day back.

The function below:

1. checks that each superseded image is still current (the *was* check);
2. closes those images;
3. adds new teams with their department links;
4. adds the new images.

Every step is written so that applying the same delivery twice changes nothing:

```python
def apply_delivery(graph, t, supersedes, new_links, new_assignments):
    with graph.begin() as tx:
        # The "was" check: every image the delivery supersedes is current,
        # or was superseded by this same delivery (a redelivery).
        found = tx.cypher("""
            FOR VALID_TIME ALL
            MATCH (a:Assignment) WHERE a.id IN $ids
              AND (a.recorded_to IS NULL OR a.recorded_to = date($t))
            RETURN count(a) AS n
        """, params={"ids": supersedes, "t": t}).to_list()[0]["n"]
        if found != len(supersedes):
            raise ValueError(f"delivery {t}: {found} of {len(supersedes)} superseded records are current")
        # Close the recording period of the superseded images.
        tx.cypher("""
            UNWIND $ids AS id
            MATCH (a:Assignment {id: id}) WHERE a.recorded_to IS NULL
            SET a.recorded_to = date($t)
        """, params={"ids": supersedes, "t": t})
        # New teams and their department links come first, so assignments can reach them.
        tx.cypher("""
            UNWIND $rows AS row
            MATCH (d:Department {id: row.department})
            MERGE (m:Team {id: row.team}) ON CREATE SET m.title = row.name
            MERGE (m)-[:PART_OF {valid_from: date(row.valid_from), recorded_from: date($t)}]->(d)
        """, params={"rows": new_links, "t": t})
        # Add the new images, each under its own id, linked to its anchors.
        tx.cypher("""
            UNWIND $rows AS row
            MATCH (e:Employee {id: row.employee}), (m:Team {id: row.team})
            MERGE (a:Assignment {id: row.id})
              ON CREATE SET a.employee = row.employee, a.team = row.team,
                            a.valid_from = date(row.valid_from), a.valid_to = date(row.valid_to),
                            a.recorded_from = date($t)
            MERGE (a)-[:OF]->(e)
            MERGE (a)-[:TO]->(m)
        """, params={"rows": new_assignments, "t": t})
```

On 2024-06-15 HR records that Ada moved from Data to a new Machine Learning
team, part of Engineering, on 2024-06-01. The delivery supersedes `ada.1:r` and
adds its closed image `ada.1:e` beside the new assignment `ada.2:r`:

```python
delivery = dict(
    t="2024-06-15",
    supersedes=["ada.1:r"],
    new_links=[{"team": "ml", "name": "Machine Learning", "department": "eng", "valid_from": "2024-06-01"}],
    new_assignments=[
        {"id": "ada.1:e", "employee": "ada", "team": "data", "valid_from": "2020-03-01", "valid_to": "2024-06-01"},
        {"id": "ada.2:r", "employee": "ada", "team": "ml", "valid_from": "2024-06-01", "valid_to": None},
    ],
)
apply_delivery(graph, **delivery)
COUNTS = "FOR VALID_TIME ALL MATCH (n) RETURN count(n) AS nodes, COUNT { ()-[]->() } AS relationships"
graph.cypher(COUNTS).to_list()
# [{'nodes': 12, 'relationships': 13}]
```

Three outcomes follow:

- A redelivery of the same day is a no-op.
- A delivery whose *was* check fails raises before writing anything.
- A delivery that fails part-way rolls back the images it had already closed.
  Here it fails on an assignment whose `valid_to` precedes its `valid_from`.

```python
apply_delivery(graph, **delivery)          # the same day, delivered again
graph.cypher(COUNTS).to_list()
# [{'nodes': 12, 'relationships': 13}]

apply_delivery(graph, "2024-06-20", ["ada.1:r"], [], [])
# ValueError: delivery 2024-06-20: 0 of 1 superseded records are current

apply_delivery(graph, "2024-06-20", ["ben.2:r"], [], [
    {"id": "ben.2:e", "employee": "ben", "team": "platform", "valid_from": "2022-01-01", "valid_to": "2021-12-31"},
])
# CypherExecutionError: Cypher execution error: node 'ben.2:e', the from bound
# 2022-01-01 ('valid_from') is after the to bound 2021-12-31 ('valid_to'), an
# inverted interval under convention 'half_open'
graph.cypher("FOR VALID_TIME ALL MATCH (a:Assignment {id: 'ben.2:r'}) RETURN a.recorded_to AS recorded_to").to_list()
# [{'recorded_to': None}]
```

Apply deliveries in the order the source recorded them, not the order a file
lists them.

The check-and-raise above is Python control flow inside the transaction. Cypher
has no `ASSERT`. A binding whose transaction is staged and cannot branch
between statements, such as Java's, has to make the check fail inside a
statement. An integer division by zero (`1 / 0`) raises and rolls the
transaction back. That is the available idiom today.

### From successive snapshots

Some sources deliver their whole table each period and say nothing about what
changed. The recording history is then the difference between each snapshot and
the images currently on record. Compare by key and by a digest of the content
columns:

- a key that is **new** gets an image added;
- a key whose content **changed** has its current image closed (its
  `recorded_to` set to the snapshot date) and a new image added, so the old
  image stays as a record of what the source said before;
- a key that **vanished** has its current image closed and nothing added;
- a snapshot delivered **twice** changes nothing, because every key then matches
  its current image;
- a snapshot **older** than the latest applied is refused, since closing and
  adding images in the past would rewrite the recording history.

Apply each snapshot in one transaction, as in the daily delivery above. The
image id is `<key>@<snapshot date>`, so a re-applied `MERGE` finds the image it
already wrote. A small `Snapshot` marker per applied date keeps the order check
honest even when a snapshot changes nothing:

```python
with graph.begin() as tx:
    current_rows = tx.cypher("""
        FOR VALID_TIME ALL
        MATCH (m:Membership) WHERE m.recorded_to IS NULL
        RETURN m.key AS key, m.id AS id, m.digest AS digest
    """).to_list()
    latest = tx.cypher("MATCH (s:Snapshot) RETURN max(s.id) AS latest").to_list()[0]["latest"]
    if latest > t:
        raise ValueError(f"snapshot {t} is older than the latest applied ({latest})")
    tx.cypher("MERGE (:Snapshot {id: $t})", params={"t": t})
    current = {r["key"]: r for r in current_rows}
    close = [r["id"] for key, r in current.items() if incoming.get(key) != r["digest"]]
    add = [row for row in rows if current.get(row["key"], {}).get("digest") != row["digest"]]
    tx.cypher("UNWIND $ids AS id MATCH (m:Membership {id: id}) SET m.recorded_to = date($t)",
              params={"ids": close, "t": t})
    tx.cypher("UNWIND $rows AS row MERGE (m:Membership {id: row.id}) ON CREATE SET ...",
              params={"rows": add, "t": t})
```

`incoming` maps each snapshot key to its digest and `rows` holds the snapshot's
images. [`examples/bitemporal_snapshots.py`](https://github.com/kkollsga/kglite/blob/main/examples/bitemporal_snapshots.py)
is the complete, runnable version.

The example runs on three month-end snapshots:

- the second ends `m1`'s validity, changes `m2`'s role and adds `m3`;
- the third drops `m1`;
- the second and third are each delivered twice.

It prints:

```text
2024-01-31 {'added': 2, 'closed': 0}
2024-02-29 {'added': 3, 'closed': 2}
2024-02-29 {'added': 0, 'closed': 0}
2024-03-31 {'added': 0, 'closed': 1}
2024-03-31 {'added': 0, 'closed': 0}
refused: snapshot 2024-02-29 is older than the latest applied (2024-03-31)
```

**Read the current images under `FOR VALID_TIME ALL`.** Under the default (valid
today), a membership whose validity has ended is hidden. A current-image query
without the prefix does not see it. Every snapshot would then find that key
"new" and add another image of it. In the example, `m2`'s validity ended in
2023:

```text
on record, valid today: ['m3']
on record, all validity: ['m2', 'm3']
```

**Bootstrap with `add_nodes`.** The first snapshot goes through `add_nodes` with
`column_types` (and `convention`) as in the set-up above. That is where the
validity pair and the recording columns get their types, and a `MERGE` cannot
declare them. Later snapshots only add images with the declared columns.

## 5. Querying

### As of an instant

The valid-time context filters every declared element, but it does not know
about images. An open-ended image superseded on the recording axis is still
valid on the valid axis.

**As known now** is the recording test `recorded_to IS NULL`. It belongs on
every image the query reads:

```python
CURRENT = """
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)
    WHERE a.recorded_to IS NULL
    RETURN e.title AS employee, t.title AS team ORDER BY employee
"""
graph.cypher(CURRENT, valid_at="2024-05-01").to_list()
# [{'employee': 'Ada', 'team': 'Data'}, {'employee': 'Ben', 'team': 'Platform'}]
graph.cypher(CURRENT, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Machine Learning'}, {'employee': 'Ben', 'team': 'Platform'}]
graph.cypher("FOR VALID_TIME AS OF date('2024-05-01')" + CURRENT).to_list()
# [{'employee': 'Ada', 'team': 'Data'}, {'employee': 'Ben', 'team': 'Platform'}]
```

`valid_at=` writes the prefix of the third query. For many questions at one
instant, freeze a view once. The fluent API carries the valid instant as its
date context and takes the recording test as a `where`:

```python
july = graph.freeze(valid_at="2024-07-01")
july.cypher(CURRENT).to_list()
# [{'employee': 'Ada', 'team': 'Machine Learning'}, {'employee': 'Ben', 'team': 'Platform'}]
current = graph.date("2024-07-01").select("Assignment").where({"recorded_to": {"is_null": True}})
sorted(current.ids())
# ['ada.2:r', 'ben.2:r']
```

A statement without a prefix runs as of today (UTC), as the fluent API does
({doc}`valid-time`, section 2.1). A question about the images themselves, such
as the audits and lineage below, reads every version. Begin it with
`FOR VALID_TIME ALL`.

### As known at an instant

**As known at** `tt` is the half-open test on the recording pair:
`recorded_from <= tt < recorded_to`, with a NULL `recorded_to` open. A recording
chain is half-open by definition, because the new image's `recorded_from` is the
old image's `recorded_to`.

Compare against `date($tt)`, not `$tt`. The stored bounds are dates, and a date
never equals a string.

Below, what a payroll run on 14 June saw for July, and what an export on 15
June saw:

```python
AS_KNOWN = """
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)
    WHERE a.recorded_from <= date($tt) AND (a.recorded_to IS NULL OR a.recorded_to > date($tt))
    RETURN e.title AS employee, t.title AS team ORDER BY employee
"""
graph.cypher(AS_KNOWN, params={"tt": "2024-06-14"}, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Data'}, {'employee': 'Ben', 'team': 'Platform'}]
graph.cypher(AS_KNOWN, params={"tt": "2024-06-15"}, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Machine Learning'}, {'employee': 'Ben', 'team': 'Platform'}]
graph.cypher(AS_KNOWN, params={"tt": "2021-06-30"}, valid_at="2021-06-30").to_list()
# [{'employee': 'Ada', 'team': 'Data'}]
```

Asked on 2021-06-30, the system did not know Ben yet. With recording instants
stored as datetimes (load them with the `'timestamp'` column type), compare
against `datetime($tt)` instead.

**The named form is the wrong tool for this axis.**
`valid_at(a, $tt, 'recorded_from', 'recorded_to')` reads a property pair that no
declaration names as **closed**. At the instant of a recording it therefore
matches both the image that ended and the one that began:

```python
graph.cypher("""
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)
    WHERE valid_at(a, $tt, 'recorded_from', 'recorded_to')
    RETURN e.title AS employee, t.title AS team ORDER BY employee, team
""", params={"tt": "2024-06-15"}, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Data'}, {'employee': 'Ada', 'team': 'Machine Learning'},
#  {'employee': 'Ben', 'team': 'Platform'}]
```

### Both axes, on every hop

The context filters the valid axis on every hop by itself. The recording test
filters only the element it names, so a hop without it reads every image.

The employee-to-department question below crosses an assignment record and the
declared `PART_OF` link, and needs the test on both. Generate the predicate from
one template rather than typing it on each hop:

```python
KNOWN = "{x}.recorded_from <= date($tt) AND ({x}.recorded_to IS NULL OR {x}.recorded_to > date($tt))"
BOTH_HOPS = f"""
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)-[p:PART_OF]->(d:Department)
    WHERE {KNOWN.format(x='a')} AND {KNOWN.format(x='p')}
    RETURN e.title AS employee, t.title AS team, d.title AS department ORDER BY employee
"""
graph.cypher(BOTH_HOPS, params={"tt": "2024-06-14"}, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Data', 'department': 'Product'},
#  {'employee': 'Ben', 'team': 'Platform', 'department': 'Engineering'}]
graph.cypher(BOTH_HOPS, params={"tt": "2024-06-15"}, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Machine Learning', 'department': 'Engineering'},
#  {'employee': 'Ben', 'team': 'Platform', 'department': 'Engineering'}]
```

Leave the test off the assignment hop and Ada comes back once per image valid on
1 July, whatever the system knew:

```python
ONE_HOP = f"""
    MATCH (e:Employee)<-[:OF]-(a:Assignment)-[:TO]->(t:Team)-[p:PART_OF]->(d:Department)
    WHERE {KNOWN.format(x='p')}
    RETURN e.title AS employee, t.title AS team, d.title AS department ORDER BY employee, team
"""
graph.cypher(ONE_HOP, params={"tt": "2024-06-15"}, valid_at="2024-07-01").to_list()
# [{'employee': 'Ada', 'team': 'Data', 'department': 'Product'},
#  {'employee': 'Ada', 'team': 'Machine Learning', 'department': 'Engineering'},
#  {'employee': 'Ben', 'team': 'Platform', 'department': 'Engineering'}]
```

This is the forgotten-hop trap that the valid-time context closes for its own
axis, back again for the modelled one.

### Lineage

An employee's assignment history joins records that never coexist. Ask it under
`FOR VALID_TIME ALL`, with only the recording test. With every version in view,
the zero-length assignment is there too:

```python
LINEAGE = f"""
    FOR VALID_TIME ALL
    MATCH (a:Assignment {{employee: $employee}})
    WHERE {KNOWN.format(x='a')}
    RETURN a.team AS team, toString(a.valid_from) AS valid_from, toString(a.valid_to) AS valid_to
    ORDER BY valid_from, valid_to
"""
graph.cypher(LINEAGE, params={"employee": "ada", "tt": "2024-06-14"}).to_list()
# [{'team': 'data', 'valid_from': '2020-03-01', 'valid_to': None}]
graph.cypher(LINEAGE, params={"employee": "ada", "tt": "2024-06-15"}).to_list()
# [{'team': 'data', 'valid_from': '2020-03-01', 'valid_to': '2024-06-01'},
#  {'team': 'ml', 'valid_from': '2024-06-01', 'valid_to': None}]
graph.cypher(LINEAGE, params={"employee": "ben", "tt": "2024-06-15"}).to_list()
# [{'team': 'data', 'valid_from': '2022-01-01', 'valid_to': '2022-01-01'},
#  {'team': 'platform', 'valid_from': '2022-01-01', 'valid_to': None}]
```

A successor relationship between anchors (a team replaced by another) is walked
the same way. Under a context, the default one included, a hop is visible only
when both its ends are valid at the one instant. The context therefore truncates
the chain. The pattern is in {doc}`valid-time`, section 6.

### Changed since, a day's delivery, and late recordings

What changed since an instant is a question on the recording axis alone: an
image that began or ended being known after it.

- A day's delivery is the same question asked for one day. It lists the images
  the day added and the images it superseded, which is enough to replay or audit
  that delivery.
- Comparing the two axes finds changes recorded more than a week after they took
  effect.

```python
graph.cypher("""
    FOR VALID_TIME ALL
    MATCH (a:Assignment)
    WHERE a.recorded_from > date($since) OR a.recorded_to > date($since)
    RETURN DISTINCT a.employee AS employee
""", params={"since": "2024-06-01"}).to_list()
# [{'employee': 'ada'}]

graph.cypher("""
    FOR VALID_TIME ALL
    MATCH (a:Assignment)
    WHERE a.recorded_from = date($day) OR a.recorded_to = date($day)
    RETURN a.id AS record,
           CASE WHEN a.recorded_to = date($day) THEN 'superseded' ELSE 'added' END AS change
    ORDER BY record
""", params={"day": "2024-06-15"}).to_list()
# [{'record': 'ada.1:e', 'change': 'added'}, {'record': 'ada.1:r', 'change': 'superseded'},
#  {'record': 'ada.2:r', 'change': 'added'}]

graph.cypher("""
    FOR VALID_TIME ALL
    MATCH (a:Assignment)
    WHERE a.recorded_from > coalesce(a.valid_to, a.valid_from) + duration({days: 7})
    RETURN a.id AS record, toString(coalesce(a.valid_to, a.valid_from)) AS effective,
           toString(a.recorded_from) AS recorded
    ORDER BY record
""").to_list()
# [{'record': 'ada.1:e', 'effective': '2024-06-01', 'recorded': '2024-06-15'},
#  {'record': 'ada.2:r', 'effective': '2024-06-01', 'recorded': '2024-06-15'}]
```

This feed survives a save and a restart because it is data. The engine's change
stream (`CALL db.cdc.*`) is process-local and never saved, so it is not a
substitute.

### Audit containment and overlap

Two queries audit the current images, each across every valid-time version:

- An assignment should lie inside the period its team belongs to a department.
- One employee's assignments should not overlap.

Both come back empty on this history:

```python
graph.cypher("""
    FOR VALID_TIME ALL
    MATCH (a:Assignment)-[:TO]->(:Team)-[p:PART_OF]->(:Department)
    WHERE a.recorded_to IS NULL AND p.recorded_to IS NULL
      AND ((p.valid_from IS NOT NULL AND (a.valid_from IS NULL OR a.valid_from < p.valid_from))
        OR (p.valid_to IS NOT NULL AND (a.valid_to IS NULL OR a.valid_to > p.valid_to)))
    RETURN a.id AS record
""").to_list()
# []

graph.cypher("""
    FOR VALID_TIME ALL
    MATCH (e:Employee)<-[:OF]-(a:Assignment), (e)<-[:OF]-(b:Assignment)
    WHERE a.id < b.id AND a.recorded_to IS NULL AND b.recorded_to IS NULL
      AND coalesce(a.valid_from, date('0001-01-01')) < coalesce(b.valid_to, date('9999-12-31'))
      AND coalesce(b.valid_from, date('0001-01-01')) < coalesce(a.valid_to, date('9999-12-31'))
    RETURN e.id AS employee, a.id AS first, b.id AS second
""").to_list()
# []
```

The zero-length `ben.1:r` does not overlap `ben.2:r`: under the half-open
reading it covers no day at all.

### The echo, and the other bindings

Each result echoes its valid-time context in `diagnostics["temporal"]`:

- the instant;
- the declared targets the filter judged;
- how many rows each hid;
- the route it took.

The recording test is ordinary `WHERE` text and does not appear there:

```python
graph.cypher(AS_KNOWN, params={"tt": "2024-06-15"}, valid_at="2024-07-01").diagnostics["temporal"]
# {'axis': 'VALID_TIME', 'source': 'explicit', 'instant': '2024-07-01', 'targets': ['(:Assignment)'],
#  'hidden': {'(:Assignment)': 2}, 'endpoint_invalid': 0, 'route': 'guarded', 'retrieval': None,
#  'slice': False, 'session_version': 24}
```

The other bindings follow the same rule:

- **Java** passes a `ValidAt` to `query`, `queryResult` or `queryBatch`. The
  batch reads one snapshot for a multi-query report.
- **MCP** `cypher_query`, `run_recipe_query` and named recipe tools take
  `valid_at` (`"all"` reads every version). A recipe takes the recording instant
  as an ordinary parameter of its own.
- **The C ABI, the `kglite` CLI and Bolt clients** take query text. Write the
  `FOR VALID_TIME AS OF` prefix into it.

On every route the recording instant is a query parameter.

## 6. Scale

The measured envelope for a graph shaped like this one is published in
{doc}`valid-time`,
[section 8](valid-time.md#8-scale-what-one-process-holds-today), and kept there.
The graph has a declared half-open interval and a hand-written recording pair on
every element.

In short:

- A million versions load in seconds and answer as-of joins in milliseconds in
  one process.
- In memory and mapped mode, a 64-million-version history does not fit one 16 GB
  process. A regional slice of it does, and the whole needs a 64 GB machine or
  shards.
- Disk mode built a 25-million-version register in one process on 16 GB.
  {doc}`large-registers` describes how.

Each superseded image is a record of its own and takes its share of that budget.
