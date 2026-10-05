# Core Concepts

## Nodes, Relationships, and Selections

**Nodes** have three built-in fields:

- `type` (primary label)
- `title` (display name)
- `id` (indexed logical identity)

They also carry arbitrary properties and optional secondary labels. Duplicate ids
are possible. Use `MERGE` or validate inputs when uniqueness matters.

**Relationships** connect two nodes with a type (e.g., `:KNOWS`) and optional
properties. The Cypher API calls them "relationships"; the fluent API calls them
"connections". They are the same thing.

**Selections** (fluent API) are lightweight views: a set of node indices that flow
through chained operations like `select().where().traverse()`. They don't copy
data.

**Atomicity.** Each `graph.cypher()` statement is atomic. If a later clause, the
deadline, or the work budget fails it, its earlier writes are rolled back.

- Use `graph.begin()` when several statements must roll back together.
- `save()` publishes a snapshot atomically.
- `open()` is write-ahead logged by default, so each committed mutation survives a
  hard crash.
- Context-managed graphs also auto-save on clean exit.
- A `with` block is not a transaction. Use `begin()` when failure must discard the
  work.

**Single-owner.** A `KnowledgeGraph` is owned by one thread at a time. Concurrent
reads are fine, but a read overlapping a mutation on the same instance raises a
clear `RuntimeError`. For multi-threaded use, pick one of three routes:

- Give each worker its own `copy()`.
- Share a read-only `graph.freeze()` snapshot for lock-free reads.
- When threads need shared reads **and** writes, use `graph.session()` (lock-free
  reads + serialized composing writes).

See {doc}`/concepts/concurrency`.

## How It Works

KGLite stores nodes and relationships in a Rust graph structure
([petgraph](https://github.com/petgraph/petgraph)). Python only sees lightweight
handles. Data converts to Python objects on access, not on query.

- **Cypher queries** parse, optimize, and execute entirely in Rust, then return
  a `ResultView`. Eligible projections keep a lazy descriptor, while ordinary
  results are already materialized in Rust (Python values convert on access).
- **Fluent API** chains build a *selection* (a set of node indices). No data is
  copied until you call `collect()`, `to_df()`, etc.
- **Persistence** uses `save()`/`load()` snapshots.
  - `open()` is crash-safe by default via a write-ahead log.
  - `durable="normal"` keeps the log without the per-commit barrier.
  - `durable="off"` opts out entirely.
  - `storage="disk"` is the exception to the crash-safe default; see
    [Choosing a storage mode](#choosing-a-storage-mode).
  - Context-managed `open()` also persists on clean exit.

## Storage Modes

KGLite has three storage backends. The Python API is identical across all three.
The trade-off is in-memory speed vs. on-disk scalability.

| Mode | Construct | Where data lives | Best for |
|---|---|---|---|
| **Default (in-memory)** | `KnowledgeGraph()` | Heap | Small / medium graphs (<5 M nodes), prototyping, fastest queries |
| **Mapped** | `KnowledgeGraph(storage="mapped")` | mmap-backed columnar files | RAM-friendly as the graph grows; same query speed as in-memory for typed lookups (O(log N) property index) |
| **Disk** | `KnowledgeGraph(storage="disk", path="/data/g")` | mmap CSR + segments | 100 M+ nodes (Wikidata-scale); kept lazy-loaded so the OS pages in only what queries touch |

Save/load works for all three. For disk mode, `save()` consolidates segment
artifacts into a top-level `disk_graph_meta.json` so `kglite.load(path)` can
reconstitute the graph.

**When optimizing**, in-memory wins. Disk and mapped exist for data that's too big
to keep on the heap. They're not "faster" backends. For Wikidata-scale workflows,
see the `load_ntriples` section of {doc}`guides/data-loading`.

### Choosing a storage mode

Start in-memory. It is the core product and the fastest path for everything that
fits in RAM.

- Reach for `mapped` only when the graph stops fitting comfortably on the heap.
- Reach for `disk` only at the Wikidata scale, where you want the OS to page data
  in lazily.

| If your graph is… | …and you want | Use | `open()` crash safety |
|---|---|---|---|
| Up to a few million nodes | Lowest latency, simplest setup | **memory** (default) | Per-commit WAL, on by default |
| Large but you still query it interactively | RAM headroom without giving up typed-lookup speed | **mapped** | Per-commit WAL; a mapped-saved `.kgl` reopens mapped (see below) |
| 100 M+ nodes / won't fit in RAM | Lazy, page-on-demand access to a huge graph | **disk** | **No WAL** — durability is your `save()` calls |

When in doubt, stay in-memory, and switch only once you hit a real RAM ceiling.
Both larger-than-RAM modes keep the identical Python and Cypher API, so moving up
is a one-line constructor change.

#### What `mapped` moves

**`mapped` moves the property columns, and nothing else.** It is not a different
graph. Nodes and relationships stay in exactly the heap structure the memory
backend uses. Only the property columns are spilled to mmap-backed files (it is
`set_memory_limit(0)` as a mode).

That explains three things:

- Typed lookups keep their speed.
- Statement rollback stays cheap (below).
- The saved `.kgl` is the same file shape. A mapped `.kgl` is read back by
  deserializing into a memory backend and then swapping it onto the mapped one, so
  reopening a mapped graph costs what reopening a memory graph costs.

Reach for it when the *properties* are what stopped fitting. It does not shrink the
topology, and it will not save a graph whose nodes and edges are the problem.

#### What `disk` changes

**`disk` is the one that changes the reload story.** A disk directory is already in
its query-ready layout, so opening it maps files instead of deserializing a
payload.

- Measured **6.5× faster to reopen at ~400k edges**.
- An external evaluation measured ~28× at 10.5M. The advantage grows with the
  graph, so treat the small-graph number as the floor rather than the rate.
- A `.kgl` (memory or mapped) has to decode its whole postcard payload before the
  first query, and that cost scales with the file.

#### Durability differs between mapped and disk

**The two larger-than-RAM modes differ in durability, not just in layout.**

- **`mapped`** keeps the same per-commit crash safety as in-memory, so growing out
  of RAM costs you nothing there.
- **`disk`** commits by publishing an immutable generation instead of by logging a
  write. `kglite.open(path, storage="disk")` opens **non-durable**. Passing any
  logging level (`durable=True`/`"full"` or `durable="normal"`) raises rather than
  pretending. A crash loses every mutation made since the last `save()`.

The `disk` guarantee is real and bounded: the published generation always survives
intact. It is *your* `save()` calls, not the engine, that decide how much a crash
can cost.

Pick `disk` for its scale, not because a graph outgrew RAM. `mapped` covers that
case with the guarantee intact. See {doc}`guides/durable-apps`.

A history table of tens of millions of versions that grows by deliveries is the
other case disk mode is built for. {doc}`guides/large-registers` covers building it
in chunks, reopening it, applying a delivery and publishing with `save()`.

#### Reopening honours the saved mode

**A saved graph records its storage mode, and reopening honours it.**

- A `.kgl` written by a mapped graph comes back mapped.
- One written by a memory graph comes back memory.
- A disk graph is a directory and always opens `disk`.

That holds with no `storage=` argument at all: the file decides. Checkpoints
written before kglite recorded the mode carry no record and keep loading as
`memory`, exactly as they always did.

#### Converting with `storage=`

**`storage=` on an existing path is a conversion request.**

- `kglite.open(path, storage="mapped")` on a memory-saved graph switches the loaded
  graph onto the mapped backend. It keeps the same nodes, edges and rows, with
  property columns moving to mmap from the next consolidation onward. The next
  `save()` records the new mode, so the graph stays mapped from then on.
- `storage="memory"` on a mapped-saved graph converts the other way.
- Neither costs a re-ingest, and neither copies the graph.

The two **disk** directions have no in-place conversion, because a disk graph *is*
its directory (CSR + mmap) rather than a payload a portable backend could adopt.
`open()` raises `kglite.ArgumentError` instead of ignoring the argument. The error
names the alternative:

- `enable_disk_mode()` moves a loaded graph onto disk storage.
- Opening the `.kgl` rather than the directory gives an in-memory graph.

Omit `storage=` to accept whatever the file provides.

This matters most for benchmarks. A create-then-reopen script that passes
`storage="mapped"` on both runs now measures the mapped backend both times. Before
the mode was recorded, it silently measured the memory backend on every run after
the first.

### `enable_disk_mode()` converts; it does not shrink the process

`enable_disk_mode()` is the memory→disk **materializer**. It builds the CSR edge
arrays in files and switches the loaded graph onto the disk backend, so everything
after it queries disk storage.

What it does *not* do is reduce this process's memory. The conversion adds the
on-disk edge structures on top of a graph that is already resident. The in-memory
structures it replaces are freed to an allocator that keeps the pages rather than
returning them. Resident memory after the call is therefore **higher**, not lower,
and stays higher for the lifetime of the process. Call `kglite.trim_memory()`
afterwards to hand the freed pages back to the OS.

The small footprint is a property of the **published directory reopened
elsewhere**. Pass the directory to `enable_disk_mode()` and it converts into that
directory and publishes it in one step. Open it in a fresh process and it starts at
roughly a tenth of the in-memory graph's resident size (measured 56 MB against
492 MB on the same graph), because its edges are paged in on demand instead of
built:

```python
graph.enable_disk_mode("graph.kgl")   # converts *and* publishes the directory
kglite.trim_memory()          # give the conversion's freed pages back

# ...in a fresh process, at ~10% of the in-memory footprint:
graph = kglite.open("graph.kgl")
```

`path` also becomes the graph's save target, exactly as `save(path)` would set it.
A later bare `save()` publishes a new generation into the same directory.

If you never want to pay the in-memory peak at all, do not convert. Build into disk
storage from the start with `KnowledgeGraph(storage="disk", path="graph.kgl")`.

#### What `graph_info()` reports after conversion

`graph_info()` reports the outcome:

- `storage_mode` becomes `"disk"`.
- `edges_mapped` becomes `True`.
- `columnar_is_mapped` stays `False`, and that is correct. It reports
  *property-column* spilling (`set_memory_limit`, or `mapped` mode), not disk-mode
  health. `False` is disk mode's normal shape rather than a failed conversion.
- `edge_property_overlay_rows` reports how many edges still hold their properties
  on the heap rather than in the mapped base. A `save()` drains it to zero.

#### Where the files land

Where the files land follows the argument.

- **With `path`**, the CSR is built inside that directory and never transits the
  system temp dir. A conversion too large for `/tmp`, or one that would cross to a
  slower device, runs where you pointed it.
- **Without `path`**, the CSR goes to a scratch directory under the system temp
  dir that is deleted when the graph drops. Nothing persists, the filesystem is
  chosen for you, and the call warns saying so.

Keep the pathless form for throwaway conversions. Use `path` for anything you
intend to keep, or build with `KnowledgeGraph(storage="disk", path=...)` to write
to the path you name from the first byte and never pay the in-memory peak.

### Statement rollback cost

**Statement rollback is O(changes) in memory and mapped mode, and for the
register-shaped statements on disk.** One mutating Cypher statement is atomic. If
it fails partway through, the graph is restored to its pre-statement state.

- **Memory and mapped graphs** use an undo journal costing O(changes). Mapped
  spills *properties* to mmap, but its node/edge graph is the same heap structure
  the memory backend uses, so the journal applies unchanged.
- **Disk graphs** hold no such structure to record an inverse edit against.
  Instead, a `MATCH … SET` of plain properties, a property `REMOVE`, `CREATE`,
  `MERGE` and `DELETE` journal each cell, title and appended row they write. They
  are undone from that journal, at a cost that follows the rows they touch.
- **Any other statement on disk** (`FOREACH`, `CALL`, `LOAD CSV`, a label change,
  `SET n += {…}`) opens a checkpoint of the disk graph before it runs. Its mapped
  arrays are mapped again and its overlays copied, and each column the statement
  writes is copied once on its first write.

If a disk graph's writes feel slow relative to its reads, check whether the
statement is one of the latter. Batching more work into fewer statements is the
lever that helps.

## Return Types

All node-related methods use a consistent key order: **`type`, `title`, `id`**, then other properties.

### Cypher

| Query type | Returns |
|-----------|---------|
| Read (`MATCH...RETURN`) | `ResultView` — lazy container, rows converted on access |
| Read with `to_df=True` | `pandas.DataFrame` |
| Mutation (for example `CREATE`/`INSERT`, `SET`/`REMOVE`, `DELETE`, or `MERGE`) | `ResultView` with `.stats` dict |
| `EXPLAIN` prefix | `ResultView` containing the structured plan (not executed) |

**Spatial return types:** `point()` values are returned as `{'latitude': float, 'longitude': float}` dicts.

### ResultView

`ResultView` is the Rust-backed result container returned by `cypher()`,
centrality methods, `collect()`, and `sample()`. Python values are converted on
access; eligible query projections may also defer row materialization.

```python
result = graph.cypher("MATCH (n:Person) RETURN n.name, n.age ORDER BY n.age")

len(result)        # row count (O(1), no conversion)
result[0]          # single row as dict (converts that row only)
result[-1]         # negative indexing works

for row in result: # iterate rows as dicts (one at a time)
    print(row)

result.head()      # first 5 rows → new ResultView
result.head(3)     # first 3 rows → new ResultView
result.tail(2)     # last 2 rows → new ResultView

result.to_list()   # all rows as list[dict] (full conversion)
result.to_df()     # pandas DataFrame (full conversion)

result.columns     # column names: ['n.name', 'n.age']
result.stats       # mutation stats (None for read queries)
```

Because `ResultView` supports iteration and indexing, it works anywhere you'd use a list of dicts — existing code that iterates over `cypher()` results continues to work unchanged.

### Node dicts

Every method that returns node data uses the same dict shape:

```python
{'type': 'Person', 'title': 'Alice', 'id': 1, 'age': 28, 'city': 'Oslo'}
#  ^^^^             ^^^^^             ^^^       ^^^ other properties
```

### Retrieval methods (cheapest to most expensive)

| Method | Returns | Notes |
|--------|---------|-------|
| `len()` | `int` | No materialization |
| `indices()` | `list[int]` | Raw graph indices |
| `ids()` | `list[Any]` | Flat list of IDs |
| `titles()` | `list[str]` | Flat list (see below) |
| `get_properties(['a','b'])` | `list[tuple]` | Flat list (see below) |
| `collect()` | `ResultView` or grouped dict | Full node dicts |
| `to_df()` | `DataFrame` | Columns: `type, title, id, ...props` |
| `node(type, id)` | `dict \| None` | Identity-index lookup |

### Flat vs. grouped results

`titles()`, `get_properties()`, and `collect()` automatically flatten when there is only one parent group (the common case). After a traversal with multiple parent groups, they return grouped dicts instead:

```python
# No traversal (single group) → flat list
graph.select('Person').titles()
# ['Alice', 'Bob', 'Charlie']

# After traversal (multiple groups) → grouped dict
graph.select('Person').traverse('KNOWS').titles()
# {'Alice': ['Bob'], 'Bob': ['Charlie']}

# Override with flatten_single_parent=False to always get grouped
graph.select('Person').titles(flatten_single_parent=False)
# {'Root': ['Alice', 'Bob', 'Charlie']}
```

### Centrality methods

All centrality methods (`pagerank`, `betweenness_centrality`, `closeness_centrality`, `degree_centrality`) return:

| Mode | Returns |
|------|---------|
| Default | `ResultView` of `{type, title, id, score}` sorted by score desc |
| `to_df=True` | `DataFrame` with columns `type, title, id, score` |
