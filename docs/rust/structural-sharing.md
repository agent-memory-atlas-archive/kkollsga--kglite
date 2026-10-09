# Structural sharing: what a write costs while a reader is alive

`DirGraph` is shared as `Arc<DirGraph>`. A lazy `ResultView`, a `freeze()`, a
`Session`, an open `Transaction` and every fluent-derived handle each hold one,
and a write needs `&mut DirGraph` — so the writer forks.

That fork used to be a deep copy of the whole graph: every node, every edge,
every index. On a 1M-node graph it cost **36.3 ms and a second 668.8 MB of
resident memory**. A write with nothing held cost 3 µs.

The copy fired on `rows = g.cypher(...)` followed by `g.cypher("… SET …")`.
It needed no threads, no snapshot API and no explicit copy. This page is the
durable record of what replaced it.

**The fork is now O(changes).** Held-view first write, 1M nodes, mean of the
timed write with the reference re-acquired untimed every round:

| graph shape | before | after |
|---|---:|---:|
| plain | 36,338 µs | **4.6 µs** |
| saved / columnar | ~17,000 µs | **4.0 µs** |
| 2 property + 1 composite + 1 range index | ~180,000 µs | **~97 µs** (all of it the range index — see [Limits](#limits)) |
| resident growth, 20 writes under a held view, 1M | +668.8 MB | **+0.0 MB** |

The first two rows are a snapshot of a distinction that no longer exists.

- When this was measured, a graph held its properties in node weights until `save()` converted it to per-type column stores.
- So "plain" and "saved / columnar" were two different objects with two different fork costs.
- Construction is columnar from the first node now, and every graph is the second row's shape.
- The measurements are kept as the record of what the overlay replaced, not as a menu of shapes to expect.

See [The rollback pre-image, since](#the-rollback-pre-image-since).

**Relationship and node deletes are O(changes) too.** `add_edge`, `remove_edge`, `remove_node` and
an edge-weight write land in the overlay's edge layer. Median of 100-200 explicit-transaction
commits, one edge or node per commit, graph with 3 edges per node, indexed `id`, release build:

| commit | 100k nodes before | after | 1M nodes before | after |
|---|---:|---:|---:|---:|
| relationship `CREATE` | 6.06 ms | **0.013 ms** | 59.7 ms | **0.015 ms** |
| relationship `DELETE` | 6.29 ms | **0.013 ms** | 60.9 ms | **0.017 ms** |
| node `DETACH DELETE` | 8.17 ms | 1.86 ms | 81.1 ms | 19.8 ms |
| relationship `CREATE`, reader held | 3.31 ms | **0.019 ms** | 32.4 ms | **0.026 ms** |
| relationship `DELETE`, reader held | 3.33 ms | **0.017 ms** | 32.8 ms | **0.023 ms** |
| node `DETACH DELETE`, reader held | 4.49 ms | 1.03 ms | 44.3 ms | 11.4 ms |
| peak resident set per commit, 1M | +389 MB | **+0 MB** | | |

The `DETACH DELETE` rows keep a cost that is not adjacency: deleting a node copies the whole
merged user index on the node's `id` (`LayeredIndex::retain_members`), 11 ms at 1M entries, and
the publish frees it. The first relationship write after a load also builds the lazy id index
(~26 ms, ~130 MB at 1M nodes) once.

---

## Why writer-side, and not the obvious alternative

The attractive design is reader-side MVCC: keep one graph, let the snapshot read
through an undo chain, and the live graph pays nothing. The journal already
captures exactly the pre-images such a read would need.

**It is not expressible in safe Rust.** The snapshot holds `Arc<DirGraph>` and
reads it as `&DirGraph` while the writer needs `&mut DirGraph` to the same
allocation. That is aliasing UB. There are two escapes, and both fail:

- A lock on the read path is categorically over the in-memory budget in the `MATCH` loop.
- A query-duration read guard makes writes *block* on a held Python object. That trades a deadlock hazard for a latency cliff.

**Rust's aliasing rules force copy-on-write to be writer-side.** This is the
structural fact behind everything below. It answers "why not just version the
reads".

---

## The mechanism

The reader's `Arc<DirGraph>` is left byte-for-byte untouched. The writer builds a
graph whose data-scale fields share the parent's allocations plus a small delta,
and folds the delta back the moment the base becomes uniquely owned again.

Four fields carry the graph-sized state, and each is layered:

| field | layer | shape |
|---|---|---|
| `graph` (the backend) | `GraphBackend::Forked` — `storage/forked.rs`, `forked_edges.rs` | base `Arc<MemoryGraph>` + node weights, an edge layer (added edges, tombstones, copied weights) and an operation log |
| `id_indices` | `storage/disk/id_index_layer.rs` | per type: `Owned` or `Layered { base: Arc<TypeEntry>, delta }`, recursive chain, deletions as `NodeIndex::end()` tombstones |
| `type_indices` | `storage/disk/type_index_layer.rs` | per type: `Vec<Arc<Vec<NodeIndex>>>`, **append-only**, last level writable |
| `property_indices`, `composite_indices` | `dir_graph/index_layer.rs` | per index: `Vec<Arc<HashMap<K, Option<Vec<NodeIndex>>>>>`, `None` = tombstone, bucket-granular copy-on-write |

Three of the four are the same idea: a stack of shared immutable levels whose
tail is writable. The differences are dictated by each field's access
pattern, not by taste:

* **`type_indices` is append-only** because a `CREATE`'s only edit is a push, and
  the realistic shape is one type holding nearly every node. A per-bucket
  copy-on-write would copy a million-entry `Vec` on the first write and win
  nothing.
* **The user index families are bucket-granular** because they are point-keyed:
  a statement touches two buckets, not the index. Their delta bucket is a *full
  copy of the merged bucket*, which is what keeps the journal's reversals
  correct unchanged (see [Invariants](#invariants)).
* **`id_indices` splits inside `Clone`** through its existing `RwLock`, because
  it can: it hands out values, not borrowed slices.
* **The layers other than `id_indices` cannot take a lock**, because their reads return `&[NodeIndex]`. They keep *every* level behind an `Arc`, including the one being written. The writer discovers the fork lazily at its next write through `Arc::get_mut`.

### The one thing that must not be done

**A `share()` that merges before it can hand back one immutable value is
O(N), and it fires on every fork that follows a write.** That is the founding
defect's own shape, since a read-then-write loop re-takes a view every
iteration. This was measured, not reasoned about: it held the 1M cell at 4.1 ms
with every other part of the design correct and every test green.

Its twin: **compaction written as "materialise into a fresh structure" is also
O(N)**, because materialising clones the base first. It reads as a fold. It
behaves as the deep clone the design removes, moved one write later. It showed
up as a +289% regression on the *dropped-view control*, not on the cell under
test.

Both are avoided the same way: the base is moved out of its `Arc`
(`Arc::try_unwrap` under a `get_mut` guard), never copied. Both are pinned
by tests that assert on pointer identity rather than on content.

---

## Invariants

**Slot identity.** Statement rollback guarantees a node or edge returns on the
exact `NodeIndex`/`EdgeIndex` it vacated. Those indices are the keys of every
index structure. `StableGraph` reuses free-list slots and offers no
index-controlled insertion, so the overlay must *predict* what `add_node` and
`add_edge` will return and reproduce it at fold-back.

`storage/slot_mirror.rs` mirrors petgraph's two free lists as LIFO stacks. It
refuses to predict, rather than guessing, for a graph whose free-list order is
not observable. That means a graph adopted by `from_graph`, unless it provably
has no holes. Unsynced means *slower*, never wrong. A
`debug_assert` validates the prediction on every insert the test suites perform.

Removals feed the same free lists, so the fold cannot replay "the appended
nodes". The overlay logs every `add_node`, `add_edge`, `remove_edge` and
`remove_node` in the order issued (`Op` in `forked.rs`) and the fold replays
that log against the base's own petgraph. Each add must come back on the slot
the overlay handed out. The same operations on the same state reproduce both
free lists and the adjacency order, which a petgraph-level test pins.

- Before anything changes, the fold simulates the log against a copy of the target's mirror and refuses on a mismatch.
- While the replay has only added things, a slot mismatch is undone newest-first and the overlay keeps serving.
- After a removal ran, a mismatch can only panic: re-adding an edge links it at the head of its lists, not where it was.

`forked_model_tests.rs` runs 12,000 seeded operation sequences against a graph
that never forks and compares every add's slot, the full read surface, and the
folded graph with its slot mirror. Two sources know the free-list order of a
loaded base. Petgraph's deserializer links both free lists in one ascending
scan, so a `.kgl` load rebuilds the mirror from the vacant slots. A
storage-mode conversion moves the same `StableDiGraph`, so it keeps the mirror
it had.

**The journal reverses into the delta, never the base.** Every `UndoEntry` is
keyed on an index and replayed through the write path. On a forked graph that
replay must land in the overlay. If any of it reached the shared base, the
reader's snapshot would silently acquire a rolled-back write, with no error and
no crash. Two specific re-pointings:

* `BucketAppended` on a node-type bucket is reversed by editing the **writable
  tail**. It *refuses* rather than guessing when the entry is not there; the
  caller then falls back to a flattening retain, which is slower and still correct. A statement's
  appends are all in the tail because a fork needs `&DirGraph` while the writer
  holds `&mut DirGraph`, so no fork can interleave with a statement.
* `BucketRemoved` re-inserts at a recorded **position**, and
  `BucketAppended` on a user index drops the **last** occurrence so a
  pre-statement occurrence of the same node is spared. Both work unchanged
  because a materialised delta bucket *is* the merged bucket the position was
  measured against.

**`supports_undo_journal()` must stay true on the forked backend.** If it were
false, every statement taken while a view is held would fall back to a
whole-graph clone checkpoint. That is an O(V+E) copy *per statement* instead of
one per fork: the fix would introduce a worse cliff than the defect.

**Depth caps.** Every layer bounds its stack at **32** levels and flattens once
at the cap. A stack only grows while a reader is held *continuously across
writes*; any write with nothing shared folds it back. The cap is not removable:
it is what bounds memory (one retained delta per level) and read-miss depth. Its
value is measured, not chosen:

- At 8 the flatten put a ~5x-median spike into one round in eight and dominated the *mean* of the held-view cell.
- At 32 the worst case is ~2x the median.

Raising it further tunes against one benchmark's hold
window rather than against a mechanism.

**Fork-private caches.** `edge_type_counts_cache` and `type_connectivity_cache`
are `ForkPrivateCache`: no `Arc`, and `Clone` returns an *empty* cache. They used
to be `Arc<RwLock<…>>` shared by a plain clone, which was a real wrong-observable
— a snapshot holder reported the *writer's* edge-type counts as its own. Making
the aliasing structurally impossible beats avoiding it. `wkt_cache` (a pure
function of its key) and `property_ndv_cache` (version-tagged, and only a planner
estimate) stay shared deliberately. `peer_counts` and the mapped lazy indexes are
likewise reset on fork: correct-but-cold beats a cache shared with a reader's
snapshot.

**Compaction contract.** Fold-back runs at write entry, which is the earliest
moment the writer can observe the reader's departure — `Arc::get_mut` succeeding
*is* that observation. So "hold a view, write, drop the view, write again"
self-heals on the very next write. A fold must be O(delta) and must decline
while any other holder is alive; the `Arc::get_mut` gate is the whole
enforcement.

**`g.copy()` forks *from* the source.** The source's own backend and the copy's
base then become the same allocation while the source is still uniquely owned.
Writing through it would edit a backend the copy is reading.
`ensure_writable()` at write entry turns the source into an overlay too. In the
steady state that costs one `Arc::get_mut` probe.

---

## Limits

These are real and deliberate; none of them is a defect.

**A delta that outgrows the base collapses.** Two thresholds, both a fraction of
the base's node and edge count with a 4,096-operation floor.

- **A fork of a fork copies the delta**, and a reader held continuously across
  commits keeps the base shared, so the delta only grows. Past a sixteenth of
  the base the next fork collapses it once (`delta_exceeds_clone_cap`).
- **An adjacency statement runs every edit twice** when the overlay carries it,
  into the delta and again at the fold. Past a sixty-fourth of the base the
  overlay collapses mid-statement and the rest runs in place
  (`delta_exceeds_write_cap`). That bounds a bulk write at the one whole-graph
  copy it paid before the overlay held adjacency. Measured at 1M nodes, 3M
  edges, statement plus commit: a `DETACH DELETE` of 100k nodes takes 573 ms
  against 560 ms before the overlay held adjacency, 500k nodes 2.47 s against
  2.39 s, and creating 100k relationships in one statement 283 ms against 267 ms.

Nothing else flattens. `vacuum`, a disk conversion, an N-Triples load and the
commit-time column reclaim collapse the overlay themselves because each needs
one concrete `StableDiGraph`.

**A continuously held reader pays an amortised flatten.** In a loop that
re-takes a view before every write, compaction never fires and the depth cap
does. On a graph with large user indexes that costs ~`|index| / 32` per fork.
It measured ~4.3 ms per write on a 1M indexed graph, against a 97 µs median
round. Read plainly, the median improves ~1,500x and the mean ~32x. The flatten
cannot be made cheaper, because it must copy a base that a reader holds.

**`range_indices` is not layered.** It is a `BTreeMap` per index, and
`lookup_range` needs ordered iteration. A level stack can only serve that by
k-way merging across levels, which is a different mechanism, not an incidental
extension. It is therefore the whole remaining fork cost on an indexed graph:
~90 µs for an index over ~1,000 distinct values. The cost is **O(distinct
values)**, so a range index over a high-cardinality property costs proportionally more.

**`unique_indices`, `embeddings` and `timeseries_store` are not layered
either.** `unique_indices` holds one entry per node of every constrained type;
`embeddings` is linear in dimension: 6.9 ms at d=64 on 1M nodes, so ≈41 ms at
d=384, and more once an HNSW index exists, whose `links` allocate per node per
layer. A graph carrying those still pays them on every fork.

**`Mapped` stays on the deep-clone path**, explicitly. **`Disk` never forks this way**: it forks through remapped immutable bases and its own mutation overlay, and none of this applies.

**The rollback pre-image clone is a different cost and is unchanged by this
work.** It is not the fork: it fires once per write *statement* on a columnar
type, with no reader held. It measured
**≈ (N / 100,000) × (368 + 41 × columns) µs**. That is linear in both axes:
≈8.6 ms per statement at 1M × 12 columns, and ≥97% of the write above N = 25,000.

On a never-saved graph the term is exactly zero. The overlay neither improves nor
worsens it: it is the statement checkpoint capturing a pre-image, not the fork
copying a graph, and the two are independent. If a write on a saved graph is
slow, check this term before suspecting the fork.

---

## The rollback pre-image, since

The paragraph above is D2's record. Its conclusion still holds: *the fork and the
statement checkpoint are independent costs*. Its **numbers do not**. The
pre-image it describes was a second `Arc` handle on the type's whole master
`ColumnStore`, so the `Arc::make_mut` at the write site deep-copied every
column of the type to change one cell.

The shape-convergence work replaced it with **cell-grained pre-images**. The
section is left standing because "unchanged by this work" was true of D2, and
the two mechanisms are easiest to tell apart side by side.

**The journal now records one entry per changed cell.** `UndoEntry::ColumnarCell`
carries `{node_type, row_id, key, prior: Option<Value>}` — the value the cell
held before the statement overwrote it — and rollback writes it straight back
into the live store. The prior value was already being read on the hot path, so
capture costs a move rather than a copy. Companion entries cover the non-cell
edits a statement can make to a store:

* `ColumnarSchemaGrown`: a `SET` that introduced a property the type lacked, which appends a null-backfilled column.
* `ColumnarRowsAppended` and `ColumnarTombstone`: the `CREATE` and `DELETE` halves, which reach a master store now that construction is columnar.
* `ColumnarTitle`.

**The mechanism is the inversion, not the size.** Because the journal holds no
handle on the store, the master stays *uniquely owned* for the whole statement,
so `Arc::make_mut` at the write site mutates one cell in place.

That direction is asserted at the write site (`columnar_write::write_column_master`)
rather than left to a benchmark to notice. The old code asserted the opposite,
that the clone *had* happened, which is how a documented invariant hid a 76–162× tax.

Two things fall out of the same change:

* An mmap-backed column is no longer materialised into the heap by the journal, so `set_memory_limit` survives writes.
* The "on a never-saved graph the term is exactly zero" escape hatch stops mattering, because there is nothing left to escape.

Cost, release build, A/B against the published 0.15.14 wheel:

* A single-row `SET` on a saved graph went **327.9 → 4.3–5.0 µs at 50 k × 12 columns and 683.5 → 4.3 µs at 100 k**.
* That is parity with a never-saved graph (ratio 0.8–1.0× at every N and column count), and flat in N rather than linear.
* Inside a transaction, 355 → 14–45 µs per statement.
* On a spilled graph, 4,961 → 4.4 µs with the spill intact.

**What the fork still copies.** One store copy per *fork*, not per statement.
The first write under a held view is 587.6 µs at 100 k × 12 (mean of first
writes). The second write is 8.2 µs, and flattening after the reader drops is
137.7 µs.

That copy is `ForkedGraph` sharing its stores with the base a reader holds: its first write
per type must copy. This is the one legitimate exception to the
uniquely-owned invariant, and the reason the write-site assertion names it.
Per-column `Arc` would narrow it to the touched column; it is a filed follow-on
with those numbers attached, not a gap.

Two residues remain, both observational no-ops:

* A cell that was *absent* before the statement is restored by writing `Value::Null`. `ColumnStore::get` and `row_properties` cannot distinguish that from absent.
* A rolled-back write whose value did not fit the column's type leaves the column demoted to `Mixed`. Values and reads are identical; only the storage tag differs, until the next consolidation re-derives it.

---

## Observing it

`GraphBackend::is_forked()` is public as a diagnostic, and bindings expose it
(`kglite._backend_is_forked` in Python). It is the one cheap, non-timing
observable that separates the three states this design moves between: flat,
forked, and folded back. Regression tests assert on it rather than on timings —
`False` where a fork is expected means whole-graph-clone semantics returned;
`True` where flat is expected means compaction stopped folding.

Engine-side the oracle is `BACKEND_CLONE_NODES` (test-only), which counts nodes
*actually copied*, so it distinguishes a genuine deep copy from the O(1) clone of
an intentionally emptied backend.
