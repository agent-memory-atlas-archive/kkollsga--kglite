# Bitemporal registers

A register that keeps history answers two questions about every fact: when was
it true in the world, and when did the register know it. This page is for the
architect of such a register, with a Java or Python ingest, who knows valid
time and transaction time already and wants to know three things: what KGLite
gives natively, what you model yourself, and what it costs. It covers set-up,
loading a register feed, the questions on both axes, the feature surface as a
checklist, and scale.

The examples build one small synthetic register shaped like the BAG: two
buildings (`Pand`), their versions (*voorkomens*) and the dwellings
(`Verblijfsobject`) inside them. The code blocks run in order as one script,
and each result shown is what that code returned; section 6 repeats the whole
example as one listing. Valid time itself, with its declarations, conventions
and fluent API, is covered in depth in {doc}`valid-time`, which this page
builds on.

## 1. Two axes: one native, one modelled

**Valid time is native.** A node label or relationship type declares the two
properties that bound its validity interval. A statement asked
`FOR VALID_TIME AS OF` an instant (or with `valid_at=`) then answers as if the
graph held only the elements valid then, on every hop, path, subquery and
algorithm, and every binding writes the same prefix. **Recording time is
modelled.** KGLite keeps no recording time of its own: when the register (or
your ingest) knew a fact is a second pair of bounds, such as `recorded_from` /
`recorded_to`, that you store on records you never overwrite and test by hand
in the query. The engine checks nothing about that pair. The rest of this page
is about doing that well.

## 2. Set-up

### Declare valid time

Declare valid time on every type that carries it, in any of the four ways
{doc}`valid-time` describes: the loaders' `validFrom` / `validTo` column types
with `convention=`, `set_temporal()`, `CALL db.temporal.declare({...})`, or a
blueprint spec's `"temporal"` key. **Choose the convention from the source.** A
register chain is half-open: BAG's *Functionele beschrijving* has each new
voorkomen begin on the date its predecessor ends. Declare it `half_open`, so
the `eind` day belongs to the successor alone. Declared `closed`, the same
rows count both sides of every boundary day, and the load warns that rows end
on the day another begins. On the RvIG municipality register the difference is
355 against 389 municipalities on 2019-01-01.

### Model recording time

Give each version the pair `recorded_from` / `recorded_to` and follow three
rules.

**Keep every superseded image as its own record.** When the register records a
change, it also closes the previous version. Until then it knew that version
as open-ended; from then on it knows it as closed. Keep both images: the
open-ended one, recorded until the change was registered, and the closed one,
recorded from then on. An extract that ships a closed version with its
`eindRegistratie` becomes two records, so the record count is higher than the
version count (about 1.47 records per voorkomen on Kadaster's Assen extract).
Do not copy `eindRegistratie` onto the final version as its `recorded_to`:
it ends the version's open-endedness, not the version.

**Give each record its own id, and keep the entity key in a separate,
non-unique property.** The example names a record `<object>.<voorkomen>:<r|e>`,
for the image *as registered* and *as ended*. Records that share an id shadow
each other: `MATCH (v {id: …})` finds one node per id and can land on an image
recorded at another time, a node load onto a stored id updates that node
instead of adding one, and the only hint is a duplicate-id warning in the
writing statement's `result.warnings`.

**Link records to a timeless anchor per object.** Relationships between
objects attach to the anchors, which is also what the BAG's product
description advises (relations with objects, not with voorkomens), so a new
version never rewrites them.

Load the recording columns with the first load, even when every value is
still NULL. A column a loader writes is a known property of the type from
then on. A property that no write has named yet is refused by a later
`CREATE` as a probable typo.

### Load the extract

The extract below holds five records. `P2` has a history: voorkomen 1 was
superseded on 2019-07-03, so it arrives as two images, `P2.1:r` and `P2.1:e`.
`P1.1` is a zero-length voorkomen, whose `begin` equals its `eind`:

```python
import kglite
import pandas as pd

pand_versions = pd.DataFrame({
    "id":            ["P1.1",       "P1.2:r",       "P2.1:r",          "P2.1:e",          "P2.2:r"],
    "pand":          ["P1",         "P1",           "P2",              "P2",              "P2"],
    "vk":            [1,            2,              1,                 1,                 2],
    "status":        ["Bouwvergunning verleend", "Bouw gestart", "Pand in gebruik", "Pand in gebruik", "Verbouwing pand"],
    "begin":         ["2020-03-01", "2020-03-01",   "1965-01-01",      "1965-01-01",      "2019-07-01"],
    "eind":          ["2020-03-01", None,           None,              "2019-07-01",      None],
    "recorded_from": ["2020-03-05", "2020-03-05",   "2010-01-01",      "2019-07-03",      "2019-07-03"],
    "recorded_to":   [None,         None,           "2019-07-03",      None,              None],
})
DECLARED = {"begin": "validFrom", "eind": "validTo",
            "recorded_from": "datetime", "recorded_to": "datetime"}

graph = kglite.KnowledgeGraph()
graph.add_nodes(pd.DataFrame({"id": ["P1", "P2"]}), "Pand", "id", "id")
graph.add_nodes(pand_versions, "PandVersie", "id", "status",
                column_types=DECLARED, convention="half_open")
# ArgumentError: Invalid argument: row 0 (0-based) of the load, the from bound
# 2020-03-01 ('begin') equals the to bound 2020-03-01 ('eind'), an empty
# interval under convention 'half_open'
```

**A zero-length version is valid at no instant under `half_open`**, so the
declaration refuses it, and so does every later load or Cypher `CREATE`,
`MERGE` or `SET` onto the declared type. The load writes nothing. Such rows
are legitimate in a register (a status corrected on the day it was set), and
no as-of question can return them, so keep them out of the declared load and
store them under a label of their own that has no declaration. The check reads
only declared types. Lineage and audit queries name both labels:

```python
empty = pand_versions["begin"] == pand_versions["eind"]
graph.add_nodes(pand_versions[~empty], "PandVersie", "id", "status",
                column_types=DECLARED, convention="half_open")
graph.add_nodes(pand_versions[empty], "PandVersieLeeg", "id", "status",
                column_types={c: "datetime" for c in DECLARED})
for label, rows in [("PandVersie", pand_versions[~empty]), ("PandVersieLeeg", pand_versions[empty])]:
    graph.add_relationships(rows[["id", "pand"]], "VAN", label, "id", "Pand", "pand")
```

Do not give such a node the declared label as a secondary label: every writer
that stamps a declared label judges the node by that label's declaration.

**On a declared relationship type, a load writes versions.** `add_relationships`
(and `replace_relationships`, `create_relationships()`, `extend()` and
blueprints) never updates a stored relationship. A row identical to one
already between its endpoints is dropped, so a redelivered file is harmless.
A row that differs in anything, whether its `eind`, its `recorded_from` or
another property, becomes a new, parallel relationship.
`connections_updated` stays 0:

```python
graph.add_nodes(pd.DataFrame({"id": ["V1"]}), "Verblijfsobject", "id", "id")
in_pand = pd.DataFrame({
    "vbo": ["V1"], "pand": ["P2"],
    "begin": ["1965-01-01"], "eind": [None],
    "recorded_from": ["2010-01-01"], "recorded_to": [None],
})
graph.add_relationships(in_pand, "IN_PAND", "Verblijfsobject", "vbo", "Pand", "pand",
                        column_types=DECLARED, convention="half_open")
redelivered = graph.add_relationships(in_pand, "IN_PAND", "Verblijfsobject", "vbo", "Pand", "pand",
                                      column_types=DECLARED, convention="half_open")
{k: redelivered[k] for k in ("connections_created", "connections_updated")}
# {'connections_created': 0, 'connections_updated': 0}
```

Close or correct a stored relationship period with Cypher `SET` when that is
what you mean. `db.temporal.declarations()` lists what is declared. Its
`abutting_rows` counts the rows whose `eind` is another row's `begin`, which
is the register chain showing through, and its `empty_rows` counts the rows
the declaration would refuse:

```python
graph.cypher("""
    CALL db.temporal.declarations() YIELD kind, name, convention, abutting_rows, empty_rows
    RETURN kind, name, convention, abutting_rows, empty_rows
""").to_list()
# [{'kind': 'node', 'name': 'PandVersie', 'convention': 'half_open', 'abutting_rows': 1, 'empty_rows': 0},
#  {'kind': 'relationship', 'name': 'IN_PAND', 'convention': 'half_open', 'abutting_rows': 0, 'empty_rows': 0}]
```

### Apply a day's delivery

A delivery closes the recording period of the images it supersedes and adds
the new ones. Apply it in one transaction, so that no reader sees the day half
applied and a failure anywhere rolls the whole day back. The function below
does four things. It checks that each superseded image is still current (the
register's *was* check), closes those images, adds the new images, and adds
the new object links. Every step is written so that applying the same
delivery twice changes nothing:

```python
def apply_delivery(graph, t, supersedes, new_versions, new_links):
    with graph.begin() as tx:
        # The "was" check: every image the delivery supersedes is current,
        # or was superseded by this same delivery (a redelivery).
        found = tx.cypher("""
            MATCH (v:PandVersie) WHERE v.id IN $ids
              AND (v.recorded_to IS NULL OR v.recorded_to = date($t))
            RETURN count(v) AS n
        """, params={"ids": supersedes, "t": t}).to_list()[0]["n"]
        if found != len(supersedes):
            raise ValueError(f"delivery {t}: {found} of {len(supersedes)} superseded images are current")
        # Close the recording period of the superseded images.
        tx.cypher("""
            UNWIND $ids AS id
            MATCH (v:PandVersie {id: id}) WHERE v.recorded_to IS NULL
            SET v.recorded_to = date($t)
        """, params={"ids": supersedes, "t": t})
        # Add the new images, each under its own id, linked to its object.
        tx.cypher("""
            UNWIND $rows AS row
            MATCH (p:Pand {id: row.pand})
            MERGE (v:PandVersie {id: row.id})
              ON CREATE SET v.pand = row.pand, v.vk = row.vk, v.status = row.status,
                            v.begin = date(row.begin), v.eind = date(row.eind),
                            v.recorded_from = date($t)
            MERGE (v)-[:VAN]->(p)
        """, params={"rows": new_versions, "t": t})
        tx.cypher("""
            UNWIND $rows AS row
            MATCH (p:Pand {id: row.pand})
            MERGE (o:Verblijfsobject {id: row.vbo})
            MERGE (o)-[:IN_PAND {begin: date(row.begin), recorded_from: date($t)}]->(p)
        """, params={"rows": new_links, "t": t})
```

On 2021-11-20 the register records that `P1` was taken into use on
2021-11-15, which ends voorkomen 2 and starts voorkomen 3, and that a new
dwelling `V2` lies in it:

```python
delivery = dict(
    t="2021-11-20",
    supersedes=["P1.2:r"],
    new_versions=[
        {"id": "P1.2:e", "pand": "P1", "vk": 2, "status": "Bouw gestart",
         "begin": "2020-03-01", "eind": "2021-11-15"},
        {"id": "P1.3:r", "pand": "P1", "vk": 3, "status": "Pand in gebruik",
         "begin": "2021-11-15", "eind": None},
    ],
    new_links=[{"vbo": "V2", "pand": "P1", "begin": "2021-11-15"}],
)
apply_delivery(graph, **delivery)
COUNTS = "MATCH (n) RETURN count(n) AS nodes, COUNT { ()-[]->() } AS relationships"
graph.cypher(COUNTS).to_list()
# [{'nodes': 11, 'relationships': 9}]
```

A redelivery of the same day is a no-op. A delivery whose *was* check fails
raises before writing anything. A delivery that fails part-way, here on a
version whose `eind` precedes its `begin`, rolls back the images it had
already closed:

```python
apply_delivery(graph, **delivery)          # the same day, delivered again
graph.cypher(COUNTS).to_list()
# [{'nodes': 11, 'relationships': 9}]

apply_delivery(graph, "2021-11-25", ["P1.2:r"], [], [])
# ValueError: delivery 2021-11-25: 0 of 1 superseded images are current

apply_delivery(graph, "2021-11-25", ["P1.3:r"], [
    {"id": "P1.4:r", "pand": "P1", "vk": 4, "status": "Pand in gebruik",
     "begin": "2021-11-25", "eind": "2021-11-24"},
], [])
# CypherExecutionError: Cypher execution error: node 'P1.4:r', the from bound
# 2021-11-25 ('begin') is after the to bound 2021-11-24 ('eind'), an empty
# interval under convention 'half_open'
graph.cypher("MATCH (v:PandVersie {id: 'P1.3:r'}) RETURN v.recorded_to AS recorded_to").to_list()
# [{'recorded_to': None}]
```

Apply deliveries in the order the register recorded them, not the order the
file lists them. The check-and-raise above is Python control flow inside the
transaction. Cypher has no `ASSERT`. A binding whose transaction is staged
and cannot branch between statements, such as Java's, has to make the check
fail inside a statement. An integer division by zero (`1 / 0`) raises and
rolls the transaction back, and that is the available idiom today.

## 3. Querying

### As of an instant

The valid-time context filters every declared element, but it does not know
about images: an open-ended image superseded on the recording axis is still
valid on the valid axis. **As known now** is the recording test
`recorded_to IS NULL`, and it belongs on every image the query reads:

```python
CURRENT = """
    MATCH (p:Pand)<-[:VAN]-(v:PandVersie)
    WHERE v.recorded_to IS NULL
    RETURN p.id AS pand, v.status AS status ORDER BY pand
"""
graph.cypher(CURRENT, valid_at="2021-12-01").to_list()
# [{'pand': 'P1', 'status': 'Pand in gebruik'}, {'pand': 'P2', 'status': 'Verbouwing pand'}]
graph.cypher(CURRENT, valid_at="2019-06-30").to_list()
# [{'pand': 'P2', 'status': 'Pand in gebruik'}]
graph.cypher("FOR VALID_TIME AS OF date('2019-06-30')" + CURRENT).to_list()
# [{'pand': 'P2', 'status': 'Pand in gebruik'}]
```

`valid_at=` writes the prefix of the third query. For many questions at one
instant, freeze a view once. The fluent API carries the valid instant as its
date context and takes the recording test as a `where`:

```python
december = graph.freeze(valid_at="2021-12-01")
december.cypher(CURRENT).to_list()
# [{'pand': 'P1', 'status': 'Pand in gebruik'}, {'pand': 'P2', 'status': 'Verbouwing pand'}]

current = graph.date("2021-12-01").select("PandVersie").where({"recorded_to": {"is_null": True}})
sorted(row["id"] for row in current.collect())
# ['P1.3:r', 'P2.2:r']
```

A statement without a prefix sees every version, and so does a client that
sends none. The fluent API defaults to today (UTC).

### As known at an instant

**As known at** `tt` is the half-open test on the recording pair:
`recorded_from <= tt < recorded_to`, with a NULL `recorded_to` open. A
registration chain is half-open by definition, because the new image's
`recorded_from` is the old image's `recorded_to`. Compare against `date($tt)`,
not `$tt`: the stored bounds are dates, and a date never equals a string.

```python
AS_KNOWN = """
    MATCH (p:Pand)<-[:VAN]-(v:PandVersie)
    WHERE v.recorded_from <= date($tt) AND (v.recorded_to IS NULL OR v.recorded_to > date($tt))
    RETURN p.id AS pand, v.status AS status ORDER BY pand
"""
graph.cypher(AS_KNOWN, params={"tt": "2021-11-19"}, valid_at="2021-12-01").to_list()
# [{'pand': 'P1', 'status': 'Bouw gestart'}, {'pand': 'P2', 'status': 'Verbouwing pand'}]
graph.cypher(AS_KNOWN, params={"tt": "2021-11-20"}, valid_at="2021-12-01").to_list()
# [{'pand': 'P1', 'status': 'Pand in gebruik'}, {'pand': 'P2', 'status': 'Verbouwing pand'}]
graph.cypher(AS_KNOWN, params={"tt": "2019-01-01"}, valid_at="2019-12-01").to_list()
# [{'pand': 'P2', 'status': 'Pand in gebruik'}]
graph.cypher(AS_KNOWN, params={"tt": "2019-07-03"}, valid_at="2019-12-01").to_list()
# [{'pand': 'P2', 'status': 'Verbouwing pand'}]
```

Asked on 2019-01-01, the register did not know `P1` yet and still knew `P2`'s
first voorkomen as open-ended. With recording instants stored as datetimes
(load them with the `'timestamp'` column type), compare against
`datetime($tt)` instead.

**The named form is the wrong tool for this axis.**
`valid_at(v, $tt, 'recorded_from', 'recorded_to')` reads a property pair that
no declaration names as **closed**, so at the instant of a registration it
matches both the image that ended and the one that began:

```python
graph.cypher("""
    MATCH (p:Pand)<-[:VAN]-(v:PandVersie)
    WHERE valid_at(v, $tt, 'recorded_from', 'recorded_to')
    RETURN p.id AS pand, v.status AS status ORDER BY pand, status
""", params={"tt": "2021-11-20"}, valid_at="2021-12-01").to_list()
# [{'pand': 'P1', 'status': 'Bouw gestart'}, {'pand': 'P1', 'status': 'Pand in gebruik'},
#  {'pand': 'P2', 'status': 'Verbouwing pand'}]
```

### Both axes, on every hop

The context filters the valid axis on every hop by itself. The recording test
filters only the element it names, so a hop without it reads every image. The
dwelling-to-building question below crosses the declared `IN_PAND` link and a
building version, and needs the test on both:

```python
BOTH_HOPS = """
    MATCH (o:Verblijfsobject)-[r:IN_PAND]->(p:Pand)<-[:VAN]-(v:PandVersie)
    WHERE r.recorded_from <= date($tt) AND (r.recorded_to IS NULL OR r.recorded_to > date($tt))
      AND v.recorded_from <= date($tt) AND (v.recorded_to IS NULL OR v.recorded_to > date($tt))
    RETURN o.id AS vbo, p.id AS pand, v.status AS status ORDER BY vbo
"""
graph.cypher(BOTH_HOPS, params={"tt": "2021-11-19"}, valid_at="2021-12-01").to_list()
# [{'vbo': 'V1', 'pand': 'P2', 'status': 'Verbouwing pand'}]
graph.cypher(BOTH_HOPS, params={"tt": "2021-11-20"}, valid_at="2021-12-01").to_list()
# [{'vbo': 'V1', 'pand': 'P2', 'status': 'Verbouwing pand'}, {'vbo': 'V2', 'pand': 'P1', 'status': 'Pand in gebruik'}]
```

Leave the test off the version hop and each dwelling comes back once per
image valid on 1 December, whatever the register knew:

```python
ONE_HOP = """
    MATCH (o:Verblijfsobject)-[r:IN_PAND]->(p:Pand)<-[:VAN]-(v:PandVersie)
    WHERE r.recorded_from <= date($tt) AND (r.recorded_to IS NULL OR r.recorded_to > date($tt))
    RETURN o.id AS vbo, p.id AS pand, v.status AS status ORDER BY vbo, status
"""
graph.cypher(ONE_HOP, params={"tt": "2021-11-20"}, valid_at="2021-12-01").to_list()
# [{'vbo': 'V1', 'pand': 'P2', 'status': 'Pand in gebruik'}, {'vbo': 'V1', 'pand': 'P2', 'status': 'Verbouwing pand'},
#  {'vbo': 'V2', 'pand': 'P1', 'status': 'Bouw gestart'}, {'vbo': 'V2', 'pand': 'P1', 'status': 'Pand in gebruik'}]
```

This is the forgotten-hop trap that the valid-time context closes for its own
axis, back again for the modelled one. Generate the predicate from one
template, as section 6 does, rather than typing it on each hop.

### Lineage

A voorkomen chain joins versions that never coexist, so ask it **without** the
context, with only the recording test. Name the zero-length label too:

```python
LINEAGE = """
    MATCH (v:PandVersie|PandVersieLeeg {pand: $pand})
    WHERE v.recorded_from <= date($tt) AND (v.recorded_to IS NULL OR v.recorded_to > date($tt))
    RETURN v.vk AS vk, v.status AS status, toString(v.begin) AS begin, toString(v.eind) AS eind
    ORDER BY vk
"""
graph.cypher(LINEAGE, params={"pand": "P1", "tt": "2021-11-19"}).to_list()
# [{'vk': 1, 'status': 'Bouwvergunning verleend', 'begin': '2020-03-01', 'eind': '2020-03-01'},
#  {'vk': 2, 'status': 'Bouw gestart', 'begin': '2020-03-01', 'eind': None}]
graph.cypher(LINEAGE, params={"pand": "P1", "tt": "2021-11-20"}).to_list()
# [{'vk': 1, 'status': 'Bouwvergunning verleend', 'begin': '2020-03-01', 'eind': '2020-03-01'},
#  {'vk': 2, 'status': 'Bouw gestart', 'begin': '2020-03-01', 'eind': '2021-11-15'},
#  {'vk': 3, 'status': 'Pand in gebruik', 'begin': '2021-11-15', 'eind': None}]
```

A successor relationship between objects (a building split or merged) is
walked the same way: under a context a hop is visible only when both its ends
are valid at the one instant, so the context truncates the chain. The pattern
is in {doc}`valid-time`, section 6.

### Changed since, and a day's delivery

What changed since an instant is a question on the recording axis alone: an
image that began or ended being known after it. A day's delivery is the same
question asked for one day. It lists the images the day added and the images
it superseded, which is enough to replay or audit that delivery:

```python
graph.cypher("""
    MATCH (v:PandVersie)
    WHERE v.recorded_from > date($since) OR v.recorded_to > date($since)
    RETURN DISTINCT v.pand AS pand
""", params={"since": "2021-11-01"}).to_list()
# [{'pand': 'P1'}]

graph.cypher("""
    MATCH (v:PandVersie)
    WHERE v.recorded_from = date($day) OR v.recorded_to = date($day)
    RETURN v.id AS image,
           CASE WHEN v.recorded_to = date($day) THEN 'superseded' ELSE 'added' END AS change
    ORDER BY image
""", params={"day": "2021-11-20"}).to_list()
# [{'image': 'P1.2:e', 'change': 'added'}, {'image': 'P1.2:r', 'change': 'superseded'},
#  {'image': 'P1.3:r', 'change': 'added'}]
```

This feed survives a save and a restart because it is data. The engine's
change stream (`CALL db.cdc.*`) is process-local and never saved, so it is not
a substitute.

### The echo, and the other bindings

Each result echoes its valid-time context in `diagnostics["temporal"]`: the
instant, the declared targets the filter judged and the route it took. The
recording test is ordinary `WHERE` text and does not appear there:

```python
graph.cypher(AS_KNOWN, params={"tt": "2021-11-20"}, valid_at="2021-12-01").diagnostics["temporal"]
# {'axis': 'VALID_TIME', 'instant': '2021-12-01', 'targets': ['(:PandVersie)'], 'route': 'guarded',
#  'retrieval': None, 'slice': False, 'session_version': 25}
```

The other bindings follow the same rule. Java passes a `ValidAt` to `query`,
`queryResult` or `queryBatch`, and the batch reads one snapshot for a
multi-query report. The MCP `cypher_query`, `run_recipe_query` and named
recipe tools take `valid_at`, and a recipe takes the recording instant as an
ordinary parameter of its own. The C ABI, the `kglite` CLI and Bolt clients
take query text, so write the `FOR VALID_TIME AS OF` prefix into it. On every
route the recording instant is a query parameter.

## 4. The feature surface

| Capability | Status | How |
|---|---|---|
| Valid-time declaration, closed or half-open | native | `validFrom` / `validTo`, `set_temporal()`, `db.temporal.declare`, blueprint `temporal` |
| As of a valid instant, every hop, path, subquery and algorithm | native | `FOR VALID_TIME AS OF`, `valid_at=`, `freeze(valid_at=)`, fluent `date()` |
| Write-time validation of valid-time bounds | native | loads and Cypher writes onto a declared type refuse an inverted or empty interval |
| Redelivery of a declared relationship type | native | identical rows dropped, differing rows become versions |
| Atomic delivery | native | `begin()` (Python), `beginTransaction()` (Java); a failure rolls the whole transaction back |
| Valid-time echo | native | `diagnostics["temporal"]`, MCP `temporal:`, Bolt `kglite.temporal` |
| Recording ("as known at") time | modelled | a second pair of properties, tested half-open by hand on every hop |
| Superseded images | modelled | one record per image, each with its own id |
| Zero-length versions | modelled | an undeclared label of their own |
| Changed since, delivery replay | modelled | queries on the recording pair |
| Reproducing an earlier answer | modelled | the recording pair, or keeping each published `.kgl` |
| Engine transaction time / audit trail | not there | `SET` overwrites and `DELETE` leaves no trace; CDC is process-local |
| `ASSERT` in a transaction | not there | raise from Python, or an integer division by zero in the statement |
| As-of on the graph-wide Python algorithm methods | not there | `pagerank()` and the other graph-wide methods ignore `date()`; run `CALL pagerank()` under `valid_at=` |
| Many valid instants in one statement | costly | `UNWIND` of instants with `valid_at(x, d)` materialises instants × rows; run one statement per instant |

## 5. Scale

The measured envelope for a register-shaped graph, with a declared half-open
interval and a hand-written recording pair on every element, is published in
{doc}`valid-time`, [section 8](valid-time.md#8-scale-what-one-process-holds-today),
and kept there. In short: a million versions load in seconds and answer as-of
joins in milliseconds in one process. Building costs about 0.9 KB per version
in every storage mode, so the full BAG with history does not fit one 16 GB
process. A municipality or region fits, and a national register needs a 64 GB
machine in memory mode or regional shards. Each superseded image is a record
of its own and takes its share of that budget.

## 6. The whole example

The listing below is the example of sections 2 and 3 as one script, run
against a fresh interpreter. It builds the recording test from one template
so every hop gets the same predicate:

```python
import kglite
import pandas as pd

# 1. The extract: objects, version images, object-level links.
pand_versions = pd.DataFrame({
    "id":            ["P1.1",       "P1.2:r",       "P2.1:r",          "P2.1:e",          "P2.2:r"],
    "pand":          ["P1",         "P1",           "P2",              "P2",              "P2"],
    "vk":            [1,            2,              1,                 1,                 2],
    "status":        ["Bouwvergunning verleend", "Bouw gestart", "Pand in gebruik", "Pand in gebruik", "Verbouwing pand"],
    "begin":         ["2020-03-01", "2020-03-01",   "1965-01-01",      "1965-01-01",      "2019-07-01"],
    "eind":          ["2020-03-01", None,           None,              "2019-07-01",      None],
    "recorded_from": ["2020-03-05", "2020-03-05",   "2010-01-01",      "2019-07-03",      "2019-07-03"],
    "recorded_to":   [None,         None,           "2019-07-03",      None,              None],
})
DECLARED = {"begin": "validFrom", "eind": "validTo",
            "recorded_from": "datetime", "recorded_to": "datetime"}
in_pand = pd.DataFrame({
    "vbo": ["V1"], "pand": ["P2"],
    "begin": ["1965-01-01"], "eind": [None],
    "recorded_from": ["2010-01-01"], "recorded_to": [None],
})

graph = kglite.KnowledgeGraph()
graph.add_nodes(pd.DataFrame({"id": ["P1", "P2"]}), "Pand", "id", "id")
graph.add_nodes(pd.DataFrame({"id": ["V1"]}), "Verblijfsobject", "id", "id")
empty = pand_versions["begin"] == pand_versions["eind"]
graph.add_nodes(pand_versions[~empty], "PandVersie", "id", "status",
                column_types=DECLARED, convention="half_open")
graph.add_nodes(pand_versions[empty], "PandVersieLeeg", "id", "status",
                column_types={c: "datetime" for c in DECLARED})
for label, rows in [("PandVersie", pand_versions[~empty]), ("PandVersieLeeg", pand_versions[empty])]:
    graph.add_relationships(rows[["id", "pand"]], "VAN", label, "id", "Pand", "pand")
graph.add_relationships(in_pand, "IN_PAND", "Verblijfsobject", "vbo", "Pand", "pand",
                        column_types=DECLARED, convention="half_open")


# 2. A day's delivery, applied in one transaction.
def apply_delivery(graph, t, supersedes, new_versions, new_links):
    with graph.begin() as tx:
        found = tx.cypher("""
            MATCH (v:PandVersie) WHERE v.id IN $ids
              AND (v.recorded_to IS NULL OR v.recorded_to = date($t))
            RETURN count(v) AS n
        """, params={"ids": supersedes, "t": t}).to_list()[0]["n"]
        if found != len(supersedes):
            raise ValueError(f"delivery {t}: {found} of {len(supersedes)} superseded images are current")
        tx.cypher("""
            UNWIND $ids AS id
            MATCH (v:PandVersie {id: id}) WHERE v.recorded_to IS NULL
            SET v.recorded_to = date($t)
        """, params={"ids": supersedes, "t": t})
        tx.cypher("""
            UNWIND $rows AS row
            MATCH (p:Pand {id: row.pand})
            MERGE (v:PandVersie {id: row.id})
              ON CREATE SET v.pand = row.pand, v.vk = row.vk, v.status = row.status,
                            v.begin = date(row.begin), v.eind = date(row.eind),
                            v.recorded_from = date($t)
            MERGE (v)-[:VAN]->(p)
        """, params={"rows": new_versions, "t": t})
        tx.cypher("""
            UNWIND $rows AS row
            MATCH (p:Pand {id: row.pand})
            MERGE (o:Verblijfsobject {id: row.vbo})
            MERGE (o)-[:IN_PAND {begin: date(row.begin), recorded_from: date($t)}]->(p)
        """, params={"rows": new_links, "t": t})


delivery = dict(
    t="2021-11-20",
    supersedes=["P1.2:r"],
    new_versions=[
        {"id": "P1.2:e", "pand": "P1", "vk": 2, "status": "Bouw gestart",
         "begin": "2020-03-01", "eind": "2021-11-15"},
        {"id": "P1.3:r", "pand": "P1", "vk": 3, "status": "Pand in gebruik",
         "begin": "2021-11-15", "eind": None},
    ],
    new_links=[{"vbo": "V2", "pand": "P1", "begin": "2021-11-15"}],
)
apply_delivery(graph, **delivery)
apply_delivery(graph, **delivery)   # a redelivery changes nothing

# 3. Questions.
KNOWN = "{x}.recorded_from <= date($tt) AND ({x}.recorded_to IS NULL OR {x}.recorded_to > date($tt))"
BOTH_AXES = f"""
    MATCH (o:Verblijfsobject)-[r:IN_PAND]->(p:Pand)<-[:VAN]-(v:PandVersie)
    WHERE {KNOWN.format(x='r')} AND {KNOWN.format(x='v')}
    RETURN o.id AS vbo, p.id AS pand, v.status AS status ORDER BY vbo
"""
for tt in ["2021-11-19", "2021-11-20"]:
    print(tt, graph.cypher(BOTH_AXES, params={"tt": tt}, valid_at="2021-12-01").to_list())

LINEAGE = f"""
    MATCH (v:PandVersie|PandVersieLeeg {{pand: $pand}})
    WHERE {KNOWN.format(x='v')}
    RETURN v.vk AS vk, v.status AS status, toString(v.begin) AS begin, toString(v.eind) AS eind
    ORDER BY vk
"""
for row in graph.cypher(LINEAGE, params={"pand": "P1", "tt": "2021-11-20"}).to_list():
    print(row)

print(graph.cypher("""
    MATCH (v:PandVersie)
    WHERE v.recorded_from > date($since) OR v.recorded_to > date($since)
    RETURN DISTINCT v.pand AS pand
""", params={"since": "2021-11-01"}).to_list())
```

It prints:

```text
2021-11-19 [{'vbo': 'V1', 'pand': 'P2', 'status': 'Verbouwing pand'}]
2021-11-20 [{'vbo': 'V1', 'pand': 'P2', 'status': 'Verbouwing pand'}, {'vbo': 'V2', 'pand': 'P1', 'status': 'Pand in gebruik'}]
{'vk': 1, 'status': 'Bouwvergunning verleend', 'begin': '2020-03-01', 'eind': '2020-03-01'}
{'vk': 2, 'status': 'Bouw gestart', 'begin': '2020-03-01', 'eind': '2021-11-15'}
{'vk': 3, 'status': 'Pand in gebruik', 'begin': '2021-11-15', 'eind': None}
[{'pand': 'P1'}]
```
