# Valid time

**Valid time is a declared property of the graph, not a filter you remember to
write: one instant governs every hop, path, algorithm and ranking, in every
binding.** Recording time, when the graph learned a fact, is modelled beside it
in the same engine ({doc}`bitemporal`).

A *valid-time* graph holds history: each version of a node or relationship
carries the period during which it was true, such as a team membership from one
transfer to the next, a licensee's share from one sale to the next, or a price
from one list to the next. KGLite asks such a graph **as of an instant** and
answers as if the graph held only the elements valid then. The reference detail
lives in the
[Cypher reference](../../reference/cypher-reference.md#statement-context-for-valid_time-as-of)
and the [fluent API reference](../../reference/fluent-api.md#temporal-filtering).

The examples share one small org chart: employees are members of teams, teams
are part of departments, employees report to managers, and each of those
periods ends on the day the next begins. Teams have lifetimes, and a successor
relationship links a team to the one that replaced it.

```python
import kglite
import pandas as pd

BOUNDS = {"valid_from": "validFrom", "valid_to": "validTo"}
employees = pd.DataFrame({"id": ["ada", "ben", "chloe", "dan", "eva"],
                          "name": ["Ada", "Ben", "Chloe", "Dan", "Eva"]})
teams = pd.DataFrame({
    "id":         ["ops",        "infra",          "platform",   "data"],
    "name":       ["Ops",        "Infrastructure", "Platform",   "Data"],
    "valid_from": ["2015-01-01", "2017-01-01",     "2022-01-01", "2018-01-01"],
    "valid_to":   ["2019-01-01", "2022-01-01",     None,         None],
})
members = pd.DataFrame({
    "employee":   ["ada",        "ada",        "ben",        "chloe",      "chloe",      "dan"],
    "team":       ["data",       "platform",   "platform",   "infra",      "platform",   "data"],
    "valid_from": ["2020-03-01", "2024-06-01", "2022-01-01", "2019-05-01", "2022-01-01", "2021-01-01"],
    "valid_to":   ["2024-06-01", None,         None,         "2022-01-01", None,         None],
})
part_of = pd.DataFrame({  # the Data team moved from Product to Engineering in 2023
    "team":       ["ops", "infra", "platform", "data",       "data"],
    "department": ["eng", "eng",   "eng",      "product",    "eng"],
    "valid_from": [None,  None,    None,       "2018-01-01", "2023-01-01"],
    "valid_to":   [None,  None,    None,       "2023-01-01", None],
})
reports_to = pd.DataFrame({
    "employee":   ["ada",        "ada",        "ben",        "chloe",      "dan"],
    "manager":    ["dan",        "chloe",      "chloe",      "eva",        "eva"],
    "valid_from": ["2020-03-01", "2024-06-01", "2022-01-01", "2019-05-01", "2021-01-01"],
    "valid_to":   ["2024-06-01", None,         None,         None,         None],
})

graph = kglite.KnowledgeGraph()
graph.add_nodes(employees, "Employee", "id", "name")
graph.add_nodes(pd.DataFrame({"id": ["eng", "product"], "name": ["Engineering", "Product"]}),
                "Department", "id", "name")
graph.add_nodes(teams, "Team", "id", "name", column_types=BOUNDS, convention="half_open")
graph.add_relationships(members, "MEMBER_OF", "Employee", "employee", "Team", "team",
                        column_types=BOUNDS, convention="half_open")
graph.add_relationships(part_of, "PART_OF", "Team", "team", "Department", "department",
                        column_types=BOUNDS, convention="half_open")
graph.add_relationships(reports_to, "REPORTS_TO", "Employee", "employee", "Employee", "manager",
                        column_types=BOUNDS, convention="half_open")
graph.add_relationships(pd.DataFrame({"team": ["ops", "infra"], "successor": ["infra", "platform"]}),
                        "SUCCEEDED_BY", "Team", "team", "Team", "successor")
```

## 1. Declare the interval

A type is *temporal* once two of its properties are declared as the bounds of
its validity interval; a NULL bound is open. Undeclared types (here
`Employee`, `Department` and `SUCCEEDED_BY`) are timeless: every query sees
them in full. Four routes record the same declaration:

- the loaders' `validFrom` / `validTo` column types, as above (`add_nodes`,
  `add_relationships`, `replace_relationships`);
- `graph.set_temporal('Role', 'start_date', 'end_date', convention='half_open')`;
- `CALL db.temporal.declare({node: 'Role', from: 'start_date', to: 'end_date', convention: 'half_open'})`,
  or `{relationship: 'MEMBER_OF', source_type: 'Employee', …}` for a
  relationship type loaded from one source node type;
- a blueprint spec's `"temporal": {"from": …, "to": …, "convention": …}` key
  (see {doc}`blueprints`).

**Choose the convention from what the `to` day means.** `closed` keeps the `to`
day valid; `half_open` makes it the first day no longer valid. The Python
routes keep the convention of a declaration the type already has and default
to `closed` otherwise; `db.temporal.declare` and a blueprint require it to be
named. When a source ends one period on the day the next begins (a transfer
that writes the new team's start as the old team's end, a licence table, a
price list), the data is half-open. Declared `closed`, such data counts both
sides of every boundary, and the load warns:

```python
import warnings

closed = kglite.KnowledgeGraph()
closed.add_nodes(employees, "Employee", "id", "name")
closed.add_nodes(teams[["id", "name"]], "Team", "id", "name")
with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    closed.add_relationships(members, "MEMBER_OF", "Employee", "employee", "Team", "team",
                             column_types=BOUNDS, convention="closed")
[str(w.message) for w in caught]
# ["2 of 6 rows of relationship type 'MEMBER_OF' end on the day another row from
# the same source node begins; under convention 'closed' both rows are valid on
# that day. If an end bound is its successor's start, declare the interval with
# convention: 'half_open'."]
ADA_TEAMS = "MATCH (:Employee {id: 'ada'})-[:MEMBER_OF]->(t:Team) RETURN t.title AS team ORDER BY team"
closed.cypher(ADA_TEAMS, valid_at="2024-06-01").to_list()   # the day of Ada's transfer
# [{'team': 'Data'}, {'team': 'Platform'}]
graph.cypher(ADA_TEAMS, valid_at="2024-06-01").to_list()
# [{'team': 'Platform'}]
```

Every headcount or total taken on a boundary day double-counts the same way.
Section 9 lists what a declaration accepts and refuses.

## 2. Ask as of an instant

Prefix a statement with `FOR VALID_TIME AS OF <instant>`, or pass `valid_at=`,
which writes the same prefix. Who was on the Platform team on 30 June 2023, and
on the day Ada joined it:

```python
PLATFORM = "MATCH (e:Employee)-[:MEMBER_OF]->(:Team {id: 'platform'}) RETURN e.title AS name ORDER BY name"
graph.cypher("FOR VALID_TIME AS OF date('2023-06-30') " + PLATFORM).to_list()
# [{'name': 'Ben'}, {'name': 'Chloe'}]
graph.cypher(PLATFORM, valid_at="2024-06-01").to_list()
# [{'name': 'Ada'}, {'name': 'Ben'}, {'name': 'Chloe'}]
```

The context filters **every** element the statement touches: each node under
every declared label it carries, each relationship under its own declaration
and only with both endpoints valid, every hop of a variable-length path,
`shortestPath`, `OPTIONAL MATCH`, `EXISTS { }` / `COUNT { }`, `text_bm25()`
statistics, `vector_score()` top-k and the graph-algorithm procedures. A query
cannot forget a hop. The Data team moved from Product to Engineering in 2023,
so the department of Ada's team depends on the instant, and one prefix dates
both hops:

```python
DEPARTMENT = """
    MATCH (:Employee {id: 'ada'})-[:MEMBER_OF]->(t:Team)-[:PART_OF]->(d:Department)
    RETURN t.title AS team, d.title AS department
"""
graph.cypher(DEPARTMENT, valid_at="2022-06-30").to_list()
# [{'team': 'Data', 'department': 'Product'}]
graph.cypher(DEPARTMENT, valid_at="2023-06-30").to_list()
# [{'team': 'Data', 'department': 'Engineering'}]
```

A manager chain is a variable-length path, judged hop by hop at the one
instant. Without a context it mixes Ada's managers from different years:

```python
CHAIN = "MATCH (:Employee {id: 'ada'})-[:REPORTS_TO*1..]->(m:Employee) RETURN m.title AS manager"
graph.cypher(CHAIN, valid_at="2023-06-30").to_list()
# [{'manager': 'Dan'}, {'manager': 'Eva'}]
graph.cypher(CHAIN, valid_at="2025-01-01").to_list()
# [{'manager': 'Chloe'}, {'manager': 'Eva'}]
graph.cypher(CHAIN).to_list()
# [{'manager': 'Chloe'}, {'manager': 'Dan'}, {'manager': 'Eva'}, {'manager': 'Eva'}]
```

A statement takes one instant; a write under a context is refused.
`valid_at=` exists on `cypher()` on `KnowledgeGraph`, `Session` (`cypher` and
`execute`), `Transaction` and `FrozenGraph`; on the MCP `cypher_query`,
`run_recipe_query` and named recipe tools; and as Java's `ValidAt` on `query`,
`queryResult` and `queryBatch`. The C ABI, the `kglite` CLI and Bolt clients
take query text: write the `FOR VALID_TIME AS OF` prefix into it. For many
queries at one instant, freeze a view once. Each result echoes the context in
`diagnostics["temporal"]` (`None` without one):

```python
as_of_2016 = graph.freeze(valid_at="2016-06-30")
as_of_2016.cypher("MATCH (t:Team) RETURN t.title AS team").to_list()
# [{'team': 'Ops'}]
graph.cypher("MATCH (t:Team) RETURN count(*) AS n", valid_at="2016-06-30").diagnostics["temporal"]
# {'axis': 'VALID_TIME', 'instant': '2016-06-30', 'targets': ['(:Team)'], 'route': 'guarded',
#  'retrieval': None, 'slice': False, 'session_version': 22}
```

**No context means every version.** A statement without the prefix sees the
whole history, as the undated manager chain shows, and so does a Neo4j-style
or GraphQL client that sends none: for "current" it must send `valid_at` =
today. `FOR VALID_TIME AS OF date()` reads today in UTC.

## 3. The fluent API: the date context

The fluent cursor carries a date context, and **it defaults to today** (UTC),
the opposite of Cypher, which never filters without a prefix:

```python
graph.select("Team").titles()                       # valid today
# ['Platform', 'Data']
graph.date("2020-06-30").select("Team").titles()    # valid on that day
# ['Infrastructure', 'Data']
graph.date("2019", "2021").select("Team").titles()  # overlapping 2019-01-01 .. 2021-12-31
# ['Infrastructure', 'Data']
graph.date("all").select("Team").titles()           # every version; temporal=False does it for one call
# ['Ops', 'Infrastructure', 'Platform', 'Data']
platform = graph.date("2023-06-30").select("Team").where({"id": "platform"})
sorted(platform.traverse("MEMBER_OF", direction="incoming").titles())
# ['Ben', 'Chloe']
```

The context is the filter the prefix runs under, so a fluent chain and the
Cypher pattern it spells return the same nodes: `traverse()` keeps a
relationship valid under its own declaration and a target node valid too, and
`expand()`, `where_connected()`, `where_orphans()`, `degrees()`,
`relationships()`, `compare()`, `to_subgraph()` and `save_subset()` follow the
same rule. `traverse(at=…)` / `traverse(during=…)` and `valid_at()` /
`valid_during()` filter explicitly. A relationship type holding several
declarations without a source type (possible only in a graph saved by an
older version) is refused, and the message names the fix:
`CALL db.temporal.undeclare({relationship: …})`, then one declaration per
`source_type`.

**The graph-wide Python methods do not read the date context.** `pagerank()`,
`betweenness_centrality()`, `louvain_communities()`,
`connected_components()`, `shortest_path()` and the other algorithm and path
methods, `vector_search()` / `search_text()` called without a selection, and
the relationship search routes (`relationship_vector_search()`,
`relationship_search_text()`, `entity="relationship"`) read the whole graph.
Ask them as of an instant through Cypher, or run many calls on
`graph.freeze(valid_at=…)`:

```python
graph.cypher(
    "CALL connected_components() YIELD node, component RETURN count(DISTINCT component) AS n",
    valid_at="2016-06-30",
).to_list()
# [{'n': 7}]
```

For relationship vectors, run `CALL db.relationship_embeddings.query(…)` under
`valid_at=`. A node `vector_search()` on a selection
(`graph.date(d).select('Doc').vector_search(…)`) ranks the selection, which the
context already filtered.

## 4. The `valid_at` / `valid_during` functions

Without a context, `valid_at(x, date)` and `valid_during(x, start, end)` test
one element against its declaration. The four- and five-argument forms name
the bound properties (closed, unless a declaration names the same pair), for a
type with no declaration or a second pair of properties:

```python
graph.cypher("""
    MATCH (t:Team) WHERE valid_at(t, date('2019-01-01'))
    RETURN t.title AS team ORDER BY team
""").to_list()
# [{'team': 'Data'}, {'team': 'Infrastructure'}]
graph.cypher("""
    MATCH (t:Team)
    WHERE valid_during(t, date('2018-06-01'), date('2019-06-01'), 'valid_from', 'valid_to')
    RETURN t.title AS team ORDER BY team
""").to_list()
# [{'team': 'Data'}, {'team': 'Infrastructure'}, {'team': 'Ops'}]
```

A query date is a `date()` or `datetime()` value, or a string read the way
those functions read it (`'2009'` is 2009-01-01); anything else (an integer,
`null`, `'garbage'`) raises `CypherExecutionError` rather than matching
nothing, and so does a stored bound that is not a date.

Use the functions where one query needs **two instants**, or bounds no
declaration names. They differ from the context in four ways:

- `valid_at(n, d)` reads **one** declaration: the node's primary type's, else
  a secondary label's. The context requires the node to be valid under
  *every* declared label it carries.
- `valid_at(r, d)` tests the relationship's own interval only. The context
  also requires both its endpoints to be valid.
- On a relationship type holding several declarations without a source type
  (only a graph saved by an older version holds one), `valid_at(r, d)` reads
  each relationship by the first declaration whose bounds it carries; the
  context refuses the type.
- Each call filters only the element it names; a hop without a call is not
  filtered at all. That is the forgotten-hop trap the context closes: date the
  membership and forget the team's department, and Ada's team belongs to both
  departments it was ever part of.

```python
graph.cypher("""
    MATCH (:Employee {id: 'ada'})-[m:MEMBER_OF]->(t:Team)-[:PART_OF]->(d:Department)
    WHERE valid_at(m, date('2022-06-30'))
    RETURN t.title AS team, d.title AS department ORDER BY department
""").to_list()
# [{'team': 'Data', 'department': 'Engineering'}, {'team': 'Data', 'department': 'Product'}]
```

## 5. Modelling history

**Anchor and fact nodes.** Give each entity one stable anchor node, declared
with its lifetime or timeless, and put each independently changing attribute
on its own declared fact node, such as
`(e)-[:HAS_TITLE]->(:JobTitle {text, lang, valid_from, valid_to})`. Attach
relationships to the anchor, so a new job title never touches them.

```python
titles = kglite.KnowledgeGraph()
titles.cypher("""
    CREATE (e:Employee {id: 'ada', title: 'Ada'}),
           (e)-[:HAS_TITLE]->(:JobTitle {text: 'Analyst', lang: 'en',
                                         valid_from: date('2020-03-01'), valid_to: date('2022-01-01')}),
           (e)-[:HAS_TITLE]->(:JobTitle {text: 'Data Engineer', lang: 'en', valid_from: date('2022-01-01')}),
           (e)-[:HAS_TITLE]->(:JobTitle {text: 'Dateningenieurin', lang: 'de', valid_from: date('2022-01-01')})
""")
titles.cypher("CALL db.temporal.declare({node: 'JobTitle', from: 'valid_from', to: 'valid_to', convention: 'half_open'})")
```

**Model B, one node per version,** suits a source that mints a new identity on
every change, such as an HR system that issues a new position id at every
reorganisation. Give each version its own id (for example the position id
plus the start date), keep the entity key in a separate, non-unique property,
and link versions with a successor relationship.

**Observations become intervals.** A series of dated observations (a headcount
on 1 January of each year) is a set of periods `[date, next date)`: set each
row's `valid_to` to the next row's date and declare `half_open`. A point fact
with `valid_from == valid_to` is empty under `half_open`: it is stored and
counted, but no as-of question returns it (section 9), while a statement
without the context (a lineage query) still reads it. To have a point fact
answer as of its own day, give it the next day as `valid_to`, or declare the
type `closed`, where `valid_from == valid_to` is a one-day interval.

**Language is a parameter, not an axis.** Pick the language in the query and
fall back with `coalesce`:

```python
titles.cypher("""
    MATCH (e:Employee {id: $id})
    OPTIONAL MATCH (e)-[:HAS_TITLE]->(own:JobTitle {lang: $lang})
    OPTIONAL MATCH (e)-[:HAS_TITLE]->(en:JobTitle {lang: 'en'})
    RETURN coalesce(own.text, en.text) AS title
""", params={"id": "ada", "lang": "fr"}, valid_at="2023-06-30").to_list()
# [{'title': 'Data Engineer'}]
```

**Close and open in one transaction,** with the same instant on both sides, so
the two periods abut exactly and no reader sees the gap:

```python
with titles.begin() as tx:
    tx.cypher(
        "MATCH (:Employee {id: $id})-[:HAS_TITLE]->(t:JobTitle {lang: $lang}) "
        "WHERE t.valid_to IS NULL SET t.valid_to = date($t)",
        params={"id": "ada", "lang": "en", "t": "2024-06-01"},
    )
    tx.cypher(
        "MATCH (e:Employee {id: $id}) "
        "CREATE (e)-[:HAS_TITLE]->(:JobTitle {text: $text, lang: $lang, valid_from: date($t)})",
        params={"id": "ada", "lang": "en", "t": "2024-06-01", "text": "Platform Engineer"},
    )
```

To audit a history for periods that fall outside their owner's lifetime or
overlap each other, see the two queries in {doc}`bitemporal`, section 5.

## 6. Lineage and two-instant questions run without a context

A successor relationship joins versions that need not coexist. The
Infrastructure team was formed in 2017 and took over from Ops in 2019, and
Platform replaced Infrastructure in 2022. Under a context a hop is visible only
when both its ends are valid at the instant, so the context truncates the
chain:

```python
LINEAGE = "MATCH (:Team {id: 'ops'})-[:SUCCEEDED_BY*1..]->(x:Team) RETURN x.title AS team"
graph.cypher(LINEAGE).to_list()
# [{'team': 'Infrastructure'}, {'team': 'Platform'}]
graph.cypher(LINEAGE, valid_at="2018-06-30").to_list()
# [{'team': 'Infrastructure'}]
graph.cypher(LINEAGE, valid_at="2023-06-30").to_list()   # Ops no longer exists
# []
```

Ask lineage, and any question comparing two instants, without a context, and
filter only the parts that need it with `valid_at(x, d)`: the unfiltered graph
for lookup, a filtered one for visibility.

```python
graph.cypher("""
    MATCH (:Team {id: 'ops'})-[:SUCCEEDED_BY*1..]->(now:Team)
    WHERE valid_at(now, date('2026-01-01'))
    RETURN now.title AS today
""").to_list()
# [{'today': 'Platform'}]
```

## 7. Valid time is not an audit trail

Valid time records when a fact was true in the world, not when the graph
learned it. `SET r.valid_to = …` overwrites the old bound, and `DELETE` leaves
no trace: KGLite keeps no recording time. To keep what was known when, store
it yourself as a second pair of bounds on records you never overwrite.
{doc}`bitemporal` covers that pattern: superseded images, one id per record,
applying a daily change feed, and the as-known-at, both-axes, lineage and
changed-since questions.

## 8. Scale: what one process holds today

Measured on this release line (release build, Apple M4, 16 GB), with every
element carrying a declared half-open interval and a hand-written recording
pair as in {doc}`bitemporal`:

- **One million versions** (a synthetic history, three versions per object,
  one relationship per version) load in about 2.2 s in every storage mode,
  take 1.5 GB of resident memory in memory and mapped mode, save to a 16 MB
  `.kgl`, and answer an as-of join in about 5 ms and a 127-instant series in
  about 17 ms (memory and mapped; disk mode pays roughly 60 ms per instant).
  A 10 000-version delivery applies in one transaction in 70–100 ms.
- **A 25-million-version register on disk.** Disk storage builds a register of
  24.7 million versions in one process on this
  machine, loaded in chunks of 500,000 versions with a `save()` after each:
  a chunk's load took 3 to 5 seconds whatever the size so far, and the footprint
  peaked at about 3.5 GB during a save and settled below 1 GB after it. The
  finished directory reopens in about 15 seconds and a few megabytes, an append
  of 1,000 rows then takes 0.02 s, and an as-of count over the whole register
  answers correctly. Disk mode has no write-ahead log (a write is durable at
  the next `save()`), and a `save()` costs what the changed types and the
  topology cost, not a fraction of the whole; {doc}`large-registers` walks the
  steps and gives the save timings.
- **A 64-million-version historical source** (64.5 million versions) does not
  fit one 16 GB process in memory or mapped mode: those modes cost about 0.9 KB
  per version when this was measured (before datetime properties were typed
  columns, which lowers it; not re-measured), so each stops between 11 and 14
  million versions, and a whole-type row-returning join retains about 0.9 KB per
  returned row. A regional slice of it (up to about 8 million versions per
  process) runs correctly in those modes with sub-second as-of queries and daily
  deliveries in seconds. The disk-mode build above is the route for a source of
  that size; the 64-million-version source itself has not been built that way.

In memory and mapped mode the per-version cost was dominated by untyped
timestamp columns, the id index and edge overflow maps, not by the interval
filter itself.

## 9. Rules the declaration enforces

**Stored bounds.** A bound may be a date, a datetime or an ISO date string, an
eight-digit `'YYYYMMDD'` string included; NULL is open. A loader's
`validFrom` / `validTo` column also converts eight-digit integers to dates as
it loads them; a declaration refuses an integer already stored.

**Property names.** A `to` property that no row carries yet (every period
still open) is accepted with a warning ("no row of … carries '…'; every row is
open-ended until one is written"). A `from` property no row carries is
refused, and so is a `to` name that is a near miss of a property the type has
(a typo). A declared bound counts as a known property of the label, so the
first `CREATE` or `MERGE` that writes the `to` is not refused as an unknown
property.

**Writes answer to the declaration.** An `add_nodes` / `add_relationships` load
onto the declared type refuses a row whose interval is inverted or whose bound
is not a date, naming the row by its 0-based position and writing nothing; a
Cypher `CREATE`, `MERGE` or `SET` refuses it naming the element, and the
statement rolls back:

```python
graph.cypher("MATCH (t:Team {id: 'data'}) SET t.valid_to = date('2010-01-01')")
# CypherExecutionError: Cypher execution error: node 'data', the from bound
# 2018-01-01 ('valid_from') is after the to bound 2010-01-01 ('valid_to'), an
# inverted interval under convention 'half_open'
graph.cypher("MATCH (t:Team {id: 'data'}) RETURN t.valid_to AS valid_to").to_list()
# [{'valid_to': None}]
```

**Empty intervals are kept.** A row whose interval is empty under `half_open`
(`valid_from == valid_to`) is valid at no instant, and is kept: the
declaration, the load or the statement that leaves it reports one warning
counting such rows and naming the first (a `UserWarning` from a load,
`result.warnings` from Cypher), and `db.temporal.declarations()` counts them
in `empty_rows`. No as-of question returns such a row.

**How a `SET` is judged.** A `SET` is judged once its clause has applied every
item, so `SET t.valid_from = …, t.valid_to = …` moves an interval in one step.
Every writer that gives a node a declared label (`add_nodes(labels=[…])`,
`add_label`, a blueprint's `labels`, ontology materialisation) judges the node
by that label's declaration too. A fluent `update()` is not judged. A bulk
load onto a declared relationship type adds each row that differs from the
stored relationships as a new version rather than updating one;
{doc}`bitemporal` shows this on a change feed.

**What `CALL db.temporal.declarations()` reports.** Every declaration with its
convention, the rows that abut at declare time (here 1 `Team`, 2 `MEMBER_OF`,
1 `PART_OF` and 1 `REPORTS_TO` row) and, counted at the graph's current state,
the rows the declaration would refuse, which only a writer the check does not
judge leaves: a fluent `update()`, an undeclare that hands a source type's
relationships to the unkeyed declaration, or a graph saved by an earlier
version.
