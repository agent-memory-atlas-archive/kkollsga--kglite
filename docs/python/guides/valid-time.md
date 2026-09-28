# Valid time

A *valid-time* graph holds history: each version of a node or relationship
carries the period during which it was true — a municipality from its founding
to its merger, a licensee's share from one transfer to the next. KGLite asks
such a graph **as of an instant** and answers as if the graph held only the
elements valid then, across every hop of the query.

This page covers the whole feature: declaring an interval, asking as of an
instant from Cypher and the fluent API, the two `valid_at` functions and how
they differ from the context, modelling history, and what valid time does not
do. The reference detail lives in the
[Cypher reference](../../reference/cypher-reference.md#statement-context-for-valid_time-as-of)
and the [fluent API reference](../../reference/fluent-api.md#temporal-filtering).

The examples share one small graph: five Dutch municipalities from the RvIG
municipality register, whose periods end on the day their successor begins.

```python
import kglite
import pandas as pd

municipalities = pd.DataFrame({
    "code": ["0001", "0053", "1966", "0003", "1979"],
    "name": ["Adorp", "Winsum", "Het Hogeland", "Appingedam", "Eemsdelta"],
    "valid_from": [None, None, "20190101", None, "20210101"],
    "valid_to": ["19900101", "20190101", None, "20210101", None],
})
graph = kglite.KnowledgeGraph()
graph.add_nodes(
    municipalities, "Municipality", "code", "name",
    column_types={"valid_from": "validFrom", "valid_to": "validTo"},
    convention="half_open",
)
graph.add_nodes(pd.DataFrame({"name": ["Groningen"]}), "Province", "name", "name")
graph.add_relationships(
    pd.DataFrame({"code": ["0001", "0053", "1966", "0003", "1979"],
                  "province": ["Groningen"] * 5}),
    "IN_PROVINCE", "Municipality", "code", "Province", "province",
)
graph.add_relationships(
    pd.DataFrame({"code": ["0001", "0053", "0003"], "successor": ["0053", "1966", "1979"]}),
    "SUCCEEDED_BY", "Municipality", "code", "Municipality", "successor",
)
```

## 1. Declare the interval

A type is *temporal* once two of its properties are declared as the bounds of
its validity interval. Undeclared types are timeless: every query sees them in
full. There are four ways to declare, and they record the same declaration:

- the loaders' `validFrom` / `validTo` column types, as above (`add_nodes`,
  `add_relationships`, `replace_relationships`);
- `graph.set_temporal('Role', 'start_date', 'end_date', convention='half_open')`;
- `CALL db.temporal.declare({node: 'Role', from: 'start_date', to: 'end_date', convention: 'half_open'})`,
  or `{relationship: 'HAS_LICENSEE', source_type: 'Field', …}` for a
  relationship type loaded from one source node type;
- a blueprint spec's `"temporal": {"from": …, "to": …, "convention": …}` key
  (see {doc}`blueprints`).

A stored bound may be a date, a datetime or an ISO date string — an
eight-digit `'YYYYMMDD'` string included; NULL is open (valid since the
beginning, or still valid). A loader's `validFrom` / `validTo` column also
converts eight-digit integers to dates as it loads them; a declaration refuses
an integer already stored.

A `to` property that no row carries yet — every period still open — is
accepted with a warning ("no row of … carries '…'; every row is open-ended
until one is written"). A `from` property no row carries is refused, and so is
a `to` name that is a near miss of a property the type has (a typo). A declared
bound counts as a known property of the label, so the first `CREATE` or `MERGE`
that writes the `to` is not refused as an unknown property.

**Choose the convention from what the `to` day means.** `closed` keeps the `to`
day valid; `half_open` makes it the first day no longer valid. The Python
routes (`add_nodes`, `set_temporal`) keep the convention of a declaration the
type already has and default to `closed` otherwise; `db.temporal.declare` and
a blueprint require the convention to be named. When a source
ends one period on the day the next begins — a register, a price list, a
licence transfer table that writes the successor's start as the predecessor's
end — the data is half-open. A closed declaration of such data counts both
sides of every boundary and warns:

```python
closed = kglite.KnowledgeGraph()
closed.add_nodes(
    municipalities, "Municipality", "code", "name",
    column_types={"valid_from": "validFrom", "valid_to": "validTo"},
    convention="closed",
)  # UserWarning: 2 of 5 rows ... end on the day another row ... begins ...
```

On the full register (1,474 municipalities) the two readings disagree on every
boundary day: on 2019-01-01, 355 municipalities exist under `half_open` and 389
under `closed`, because 34 of them end that day.

**The declaration validates the rows it finds, and later writes answer to the
same rule.** An `add_nodes` / `add_relationships` load onto the declared type
refuses a row whose interval is inverted, empty under `half_open`, or whose
bound is not a date, naming the row by its 0-based position and writing
nothing; a Cypher `CREATE`, `MERGE` or `SET` refuses it naming the element,
and the statement rolls back:

```python
graph.cypher(
    "MATCH (m:Municipality {code: '0001'}) SET m.valid_to = date('1800-01-01')"
)  # CypherExecutionError: node '0001', the from bound ... is after the to bound ...
```

A `SET` is judged once its clause has applied every item, so
`SET m.valid_from = …, m.valid_to = …` moves an interval in one step. NULL
bounds stay open. Every writer that gives a node a declared label —
`add_nodes(labels=[…])`, `add_label`, a blueprint's `labels`, ontology
materialisation — judges the node by that label's declaration too. A fluent
`update()` is not judged. A bulk load onto a declared relationship type adds
each row that differs from the stored relationships as a new version, rather
than updating one; {doc}`bitemporal` shows this on a register feed.

`CALL db.temporal.declarations()` lists every declaration with its convention,
the rows that abut at declare time and, counted at the graph's current state,
the rows the declaration would refuse — which only a writer the check does not
judge leaves: a fluent `update()`, an undeclare that hands a source type's
relationships to the unkeyed declaration, or a graph saved by an earlier
version.

## 2. Ask as of an instant

Prefix a statement with `FOR VALID_TIME AS OF <instant>`, or pass `valid_at=`,
which writes the same prefix:

```python
graph.cypher(
    "FOR VALID_TIME AS OF date('2020-12-31') "
    "MATCH (m:Municipality)-[:IN_PROVINCE]->(p:Province) RETURN m.title AS name ORDER BY name"
).to_list()
# [{'name': 'Appingedam'}, {'name': 'Het Hogeland'}]

graph.cypher(
    "MATCH (m:Municipality)-[:IN_PROVINCE]->(p:Province) RETURN m.title AS name ORDER BY name",
    valid_at="2021-01-01",
).to_list()
# [{'name': 'Eemsdelta'}, {'name': 'Het Hogeland'}]
```

The context filters **every** element the statement touches — each node under
every declared label it carries, each relationship under its own declaration
and only with both endpoints valid, every hop of a variable-length path,
`shortestPath`, `OPTIONAL MATCH`, `EXISTS { }` / `COUNT { }`, `text_bm25()`
statistics, `vector_score()` top-k and the graph-algorithm procedures. A query
cannot forget a hop. A statement takes one instant; a write under a context is
refused.

`valid_at=` exists on these entry points: `cypher()` on `KnowledgeGraph`,
`Session` (`cypher` and `execute`), `Transaction` and `FrozenGraph`; the MCP
`cypher_query`, `run_recipe_query` and named recipe tools; Java's `ValidAt` on
`query`, `queryResult` and `queryBatch`. The C ABI, the `kglite` CLI and Bolt
clients take query text: write the `FOR VALID_TIME AS OF` prefix into it. For
many queries at one instant, freeze a view once:

```python
as_of_2020 = graph.freeze(valid_at="2020-06-30")
as_of_2020.node_count()   # 3: the 2 municipalities valid then and the timeless province
as_of_2020.cypher("MATCH (m:Municipality) RETURN count(*) AS n").to_list()  # [{'n': 2}]
```

Each result echoes the context in `diagnostics["temporal"]` (the instant, the
declared targets the statement reached and the route it took; `None` without a
context):

```python
graph.cypher("MATCH (m:Municipality) RETURN count(*) AS n", valid_at="2020-06-30").diagnostics["temporal"]
```

**No context means every version.** A statement without the prefix sees the
whole history, and so does a Neo4j-style or GraphQL client that sends none: for
"current" it must send `valid_at` = today. `FOR VALID_TIME AS OF date()` reads
today in UTC.

## 3. The fluent API: the date context

The fluent cursor carries a date context, and **it defaults to today** (UTC) —
the opposite of Cypher, which never filters without a prefix:

```python
graph.select("Municipality").collect()              # valid today: Het Hogeland, Eemsdelta
graph.date("2020-12-31").select("Municipality")     # valid on that day
graph.date("2019", "2020")                          # overlapping 2019-01-01 .. 2020-12-31
graph.date("all").select("Municipality")            # every version
graph.select("Municipality", temporal=False)        # every version, for one call
```

The context is the filter the prefix runs under, so a fluent chain and the
Cypher pattern it spells return the same nodes: `traverse()` keeps a
relationship valid under its own declaration and a target node valid too, and
`expand()`, `where_connected()`, `where_orphans()`, `degrees()`,
`relationships()`, `compare()`, `to_subgraph()` and `save_subset()` follow the
same rule. `traverse(at=…)` / `traverse(during=…)` and `valid_at()` /
`valid_during()` filter explicitly. A relationship type holding several
declarations without a source type — possible only in a graph saved by an
older version — is refused, and the message names the fix:
`CALL db.temporal.undeclare({relationship: …})`, then one declaration per
`source_type`.

**The graph-wide Python methods do not read the date context.**
`pagerank()`, `betweenness_centrality()`, `louvain_communities()`,
`connected_components()`, `shortest_path()` and the other algorithm and path
methods, `vector_search()` / `search_text()` called without a selection, and
the relationship search routes (`relationship_vector_search()`,
`relationship_search_text()`, `entity="relationship"`) read the whole graph
whatever the selection or `date()` says. Ask them as of an instant through
Cypher:

```python
graph.cypher(
    "CALL connected_components() YIELD node, component RETURN count(DISTINCT component) AS n",
    valid_at="2020-06-30",
).to_list()
```

`graph.freeze(valid_at=…).cypher("CALL pagerank() …")` does the same for many
calls; for relationship vectors, run `CALL db.relationship_embeddings.query(…)`
under `valid_at=`. A node `vector_search()` on a selection
(`graph.date(d).select('Doc').vector_search(…)`) ranks the selection, which the
context already filtered.

## 4. The `valid_at` / `valid_during` functions

Without a context, `valid_at(x, date)` and `valid_during(x, start, end)` test
one element against its declaration, and the four- and five-argument forms name
the bound properties (closed, unless a declaration names the same pair):

```python
graph.cypher("""
    MATCH (m:Municipality)
    WHERE valid_at(m, date('2019-01-01'))
    RETURN count(*) AS n
""").to_list()   # [{'n': 2}]: Het Hogeland and Appingedam
```

Name the bounds when the type has no declaration, or to test a second pair of
properties; the interval overlap test is `valid_during`:

```python
graph.cypher("""
    MATCH (m:Municipality)
    WHERE valid_during(m, date('2018-06-01'), date('2019-06-01'), 'valid_from', 'valid_to')
    RETURN m.title AS name ORDER BY name
""").to_list()   # Appingedam, Het Hogeland, Winsum
```

A query date is a `date()` or `datetime()` value, or a string read the way
those functions read it (`'2009'` is 2009-01-01); anything else — an integer,
`null`, `'garbage'` — raises `CypherExecutionError` rather than matching
nothing, and so does a stored bound that is not a date.

Use the functions where one query needs **two instants**, or bounds no
declaration names. They are not the context, and they differ from it in four ways:

- `valid_at(n, d)` reads **one** declaration — the node's primary type's, else
  a secondary label's. The context (and the fluent filters) require the node
  to be valid under *every* declared label it carries.
- `valid_at(r, d)` tests the relationship's own interval only. The context
  also requires both its endpoints to be valid.
- On a relationship type holding several declarations without a source type
  (only a graph saved by an older version holds one), `valid_at(r, d)` reads
  each relationship by the first declaration whose bounds it carries. The
  context and the fluent filters refuse the type.
- Each call filters only the element it names; a hop without a call is not
  filtered at all. That is the forgotten-hop trap the context closes: on a
  licence history, dating the operator hop of a partner query and forgetting
  the two licensee hops returns every partner the fields ever had (18/11/18/20/19)
  instead of the partners at the date (4/1/4/4/2).

## 5. Modelling history

**Anchor and fact nodes.** Give each entity one stable anchor node — declared
with its lifetime, or timeless — and put each independently changing attribute
on its own declared fact node: `(m)-[:HAS_NAME]->(:Name {text, lang, valid_from, valid_to})`.
Attach relationships to the anchor, so a rename never touches them.

```python
names = kglite.KnowledgeGraph()
names.cypher("""
    CREATE (m:Municipality {id: '0003', title: 'Appingedam', valid_from: null, valid_to: date('2021-01-01')}),
           (m)-[:HAS_NAME]->(:Name {text: 'Appingedam', lang: 'nl', valid_from: null, valid_to: null}),
           (m)-[:HAS_NAME]->(:Name {text: 'Dam', lang: 'gos', valid_from: date('1990-01-01'), valid_to: null})
""")
names.cypher("CALL db.temporal.declare({node: 'Name', from: 'valid_from', to: 'valid_to', convention: 'half_open'})")
```

Every name here is still current, so no row holds `valid_to` yet: the
declaration is accepted with a warning that every row is open-ended until one
is written.

**Model B, one node per version,** suits a source that mints a new identity on
every change — a register whose rename gets a new code. Give each version its
own id (for example the code plus the start date), keep the entity key in a
separate, non-unique property, and link versions with a successor
relationship.

**Observations become intervals.** A series of dated observations (a count on
1 January of each year) is a set of periods `[date, next date)`: set each row's
`valid_to` to the next row's date and declare `half_open`. A point fact with
`valid_from == valid_to` would be empty under `half_open`: the declaration (or
a `validFrom` / `validTo` load) refuses such a row, naming it, and so does a
later load or Cypher write onto the declared type. Only a fluent `update()`
or a graph saved by an earlier version can hold one on a node, and
`db.temporal.declarations()` then counts it in `empty_rows`.

**Language is a parameter, not an axis.** Pick the language in the query and
fall back with `coalesce`:

```python
names.cypher("""
    MATCH (m:Municipality {id: $code})
    OPTIONAL MATCH (m)-[:HAS_NAME]->(own:Name {lang: $lang})
    OPTIONAL MATCH (m)-[:HAS_NAME]->(nl:Name {lang: 'nl'})
    RETURN coalesce(own.text, nl.text) AS name
""", params={"code": "0003", "lang": "fy"}, valid_at="2020-06-30").to_list()
# [{'name': 'Appingedam'}]
```

**Close and open in one transaction,** with the same instant on both sides, so
the two periods abut exactly and no reader sees the gap:

```python
with names.begin() as tx:
    tx.cypher(
        "MATCH (:Municipality {id: $code})-[:HAS_NAME]->(n:Name {lang: $lang}) "
        "WHERE n.valid_to IS NULL SET n.valid_to = date($t)",
        params={"code": "0003", "lang": "gos", "t": "2020-06-01"},
    )
    tx.cypher(
        "MATCH (m:Municipality {id: $code}) "
        "CREATE (m)-[:HAS_NAME]->(:Name {text: $name, lang: $lang, valid_from: date($t)})",
        params={"code": "0003", "lang": "gos", "t": "2020-06-01", "name": "Dam (new spelling)"},
    )
```

**Audit containment and overlap with two queries.** A membership should lie
inside its member's lifetime, and one member's periods of one relationship type
should not overlap:

```python
graph.cypher("""
    MATCH (m:Municipality)-[r:IN_PROVINCE]->(:Province)
    WHERE (r.valid_from IS NOT NULL AND m.valid_from IS NOT NULL AND r.valid_from < m.valid_from)
       OR (m.valid_to IS NOT NULL AND (r.valid_to IS NULL OR r.valid_to > m.valid_to))
    RETURN m.id AS code, r.valid_from AS from, r.valid_to AS to
""").to_list()   # the undated memberships of the example outlive 0001, 0053 and 0003

graph.cypher("""
    MATCH (:Province)<-[a:IN_PROVINCE]-(m:Municipality)-[b:IN_PROVINCE]->(:Province)
    WHERE id(a) < id(b)
      AND coalesce(a.valid_from, date('0001-01-01')) < coalesce(b.valid_to, date('9999-12-31'))
      AND coalesce(b.valid_from, date('0001-01-01')) < coalesce(a.valid_to, date('9999-12-31'))
    RETURN m.id AS code
""").to_list()   # []
```

## 6. Lineage and two-instant questions run without a context

A successor relationship joins versions that need not coexist. Adorp (0001)
merged into Winsum (0053) in 1990, and Winsum into Het Hogeland (1966) in 2019.
Under a context a hop is visible only when both its ends are valid at the
instant, so the context truncates the chain:

```python
chain = "MATCH (:Municipality {id: '0001'})-[:SUCCEEDED_BY*1..]->(x) RETURN x.id AS code"
graph.cypher(chain).to_list()                          # 0053, 1966
graph.cypher(chain, valid_at="1989-06-30").to_list()   # 0053 only
graph.cypher(chain, valid_at="2020-01-01").to_list()   # none: Adorp no longer exists
```

Ask lineage, and any question comparing two instants, without a context, and
filter only the parts that need it with `valid_at(x, d)` — use the unfiltered
graph for lookup and a filtered one for visibility:

```python
graph.cypher("""
    MATCH (old:Municipality {id: '0001'})-[:SUCCEEDED_BY*1..]->(now:Municipality)
    WHERE valid_at(now, date('2026-01-01'))
    RETURN now.title AS today
""").to_list()   # [{'today': 'Het Hogeland'}]
```

## 7. Valid time is not an audit trail

Valid time records when a fact was true in the world, not when the graph
learned it. `SET r.valid_to = …` overwrites the old bound, and `DELETE` leaves
no trace: KGLite keeps no recording time. To keep what was known when, store
it yourself as a second pair of bounds on records you never overwrite.
{doc}`bitemporal` covers that pattern: superseded images, one id per record,
loading a register feed, and the as-known-at, both-axes, lineage and
changed-since questions.

## 8. Scale: what one process holds today

Measured on this release line (release build, Apple M4, 16 GB), with every
element carrying a declared half-open interval and a hand-written recording
pair as in {doc}`bitemporal`:

- **One million versions** (a synthetic register, three versions per object,
  one relationship per version) load in about 2.2 s in every storage mode,
  take 1.5 GB of resident memory in memory and mapped mode, save to a 16 MB
  `.kgl`, and answer an as-of join in about 5 ms and a 127-instant series in
  about 17 ms (memory and mapped; disk mode pays roughly 60 ms per instant).
  A 10 000-version delivery applies in one transaction in 70–100 ms.
- **The real BAG register** (Kadaster's extract with history, 64.5 million
  versions) does not fit one 16 GB process: building costs about 0.9 KB per
  version in every storage mode, so each mode stops between 11 and 14 million
  versions, and a whole-type row-returning join retains about 0.9 KB per
  returned row. A single municipality or region — up to about 8 million
  versions per process — runs correctly with sub-second as-of queries and
  daily deliveries in seconds; a national twin needs either a 64 GB machine in
  memory mode or regional shards.
- Disk mode is a read substrate. A write statement copies each column it
  writes into memory once (about 0.4 ms for a 2-million-row integer column)
  and then costs about 1.2 µs per row whatever the type's size — a 2,000-row
  close on a 2-million-node disk type applies in about 3 ms — but there is no
  write-ahead log (a write is durable at the next `save()`), and appending
  rows to a reopened type re-materialises that type in memory. Serve from
  disk; ingest in memory or mapped mode.

The per-version cost is dominated by untyped timestamp columns, the id index
and edge overflow maps, not by the interval filter itself; the storage work
that would let a register-sized graph build and serve on 16 GB is scoped in
the project's backlog, and the numbers above are the honest envelope until it
ships.
