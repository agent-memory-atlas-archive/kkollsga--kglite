# Durable embedded apps

This guide covers running KGLite as the **embedded database behind an
application**. It covers three things:

- the open → mutate → reopen lifecycle
- persistence on close
- crash-safe durable writes via a write-ahead log (WAL)

If you only build a graph, query it, and throw it away, you don't need any of
this. `KnowledgeGraph()` plus {doc}`data-loading` is enough.

Reach for this guide when the graph is *long-lived state* your app reopens across
runs. Examples are an agent's memory, a knowledge base that accretes facts, and a
service that accepts writes between restarts.

## The lifecycle entry points

| Call | What it does |
|---|---|
| `kglite.open(path)` | **Load-or-create.** Opens the graph at `path` if it exists, creates a fresh one bound to `path` if it doesn't. The database-style entry point. |
| `kglite.load(path)` | Load an existing `.kgl` file (or disk-mode directory). Raises `kglite.FileError` if missing, `kglite.FileFormatError` if corrupt (see below). |
| `g.save(path=None, *, fsync=True)` | Write a full checkpoint, **atomically and durably**. With no `path`, saves back to the remembered path. |
| `g.to_bytes()` / `kglite.from_bytes(data)` | Serialize/deserialize the graph to/from a `.kgl` **byte buffer** — own the write (object storage, a pipe, a checksum) instead of a filesystem path. |
| `g.close()` | Checkpoint, release persistence ownership, and retain a detached mutable graph. A failed checkpoint keeps ownership for retry. |
| `with kglite.open(...) as g:` | Checkpoint on clean exit; skip checkpoint on exception. Both detach the retained graph. Already committed WAL writes remain recoverable. |

The **remembered path** ties these together. `open()` and `load()` record where
the graph came from. A later bare `save()`, or the context manager's auto-save,
writes back without you re-specifying the target.

```python
import kglite

# First run: file doesn't exist → fresh graph, bound to "app.kgl".
with kglite.open("app.kgl") as g:
    g.cypher("CREATE (:Person {id: 1, name: 'Alice'})")
# clean exit → auto-saved to app.kgl

# Next run: file exists → loaded back.
with kglite.open("app.kgl") as g:
    g.cypher("CREATE (:Person {id: 2, name: 'Bob'})")
    print(g.cypher("MATCH (p:Person) RETURN count(p) AS n").scalar())  # 2
```

### After `close()` or context exit

The retained graph keeps its data, query configuration and CDC stream. It has no
remembered path, WAL writer or writer lease. It remains queryable and mutable, and
those later writes are private.

- `save()` without a path refuses.
- `save(path)` explicitly persists the detached snapshot, using the existing
  unlocked snapshot-save contract.
- Held read snapshots remain readable without blocking the next writer.
- Transactions begun under the ended owner cannot commit their writes back into
  it. Use a new transaction on the detached graph, or reopen the database for
  further durable work.
- A failed close retains ownership and its save target for retry. Durable
  checkpoint preparation can still invalidate an already-open transaction through
  normal conflict detection. Retry that work in a fresh transaction.

### Atomic saves

**Every `save()` is atomic and torn-proof**, even in non-durable mode. It writes
to a sibling temp file and atomically renames it over the target. A crash
mid-save can never leave a half-written `.kgl`: a reader always sees the old file
or the complete new one. This removes the temp-file + `os.replace` + dir-fsync
dance consumers used to hand-roll.

- With `fsync=True` (default), the file and its directory are flushed to physical
  storage before returning.
- Pass `fsync=False` to skip that flush for speed in a hot loop. The save is
  still atomic.

### Corrupt-file detection

**Corrupt-file detection is typed.**

- `load()` / `from_bytes()` raise `kglite.FileFormatError` (a subclass of
  `kglite.KgError`) on a corrupt, truncated, or wrong-format input.
- `load()` raises `kglite.FileError` on a missing file.

A disposable-cache consumer can therefore branch "corrupt → rebuild from source"
vs "missing → create new" cleanly, without a broad `except IOError`.

Detection does not rely on the damage happening to be structurally invalid. Every
section of a `.kgl` is written with a checksum and verified on load. The sections
are topology, each node type's columns, embeddings, time series, secondary
labels, and the vector index. A bit that flips in storage or transit produces an
error naming the damaged section, rather than a graph that loads with quietly
different data.

Files written by older versions lack the checksums and load unchanged. Files
written by this version load on older versions.

## The default: crash-safe

`open()` is durable by default. Every committed mutation is `fsync`'d before the
call returns, so a mutation that has returned survives a hard crash.

```python
g = kglite.open("app.kgl")
g.cypher("CREATE (:Order {id: 1001, total: 49.90})")   # fsync'd before this returns
```

You get this without asking for it because it is what makes an embedded database
trustworthy. The alternative default silently loses every write since the last
explicit `save()` whenever a process dies.

The default is the *strongest* level, not the only one. If power-loss safety is
more than your application needs, `durable="normal"` keeps the log and drops only
the per-commit barrier. See [Choosing a durability
level](#choosing-a-durability-level).

## Choosing a durability level

`durable` names **what a committed mutation survives**. It uses SQLite's
`synchronous` vocabulary. The levels are stated as guarantees rather than as
syscalls: the syscall differs by platform, the guarantee does not.

| `durable=` | A committed mutation survives… | Per-commit cost |
|---|---|---|
| `"full"` (or `True`) — **default** | process crash, OS crash, **power loss** | one barrier |
| `"normal"` | **process crash** — `kill -9`, panic, OOM-kill | no barrier |
| `"off"` (or `False`) | nothing since the last `save()` | no log |

`True` and `False` are accepted spellings of `"full"` and `"off"`, so existing
code keeps working unchanged.

### `"normal"` — the process-crash level

```python
g = kglite.open("app.kgl", durable="normal")
g.cypher("CREATE (:Order {id: 1001, total: 49.90})")   # logged, not barriered
```

The frame is handed to the kernel with a plain write before the call returns.
**The page cache belongs to the kernel, not to your process.** The commit
therefore survives your process dying by any means: an uncaught exception,
`kill -9`, the OOM killer. It does not survive the *kernel* dying. An OS crash or
a power cut loses commits made since the last `save()`.

That is the right trade for most applications. A crashing process is the failure
that actually happens, and a power cut is the one you keep backups for.

It is also the level to reach for when per-commit barrier latency is shaping your
write throughput. `"normal"` writes the same log frame and skips only the
barrier.

### `"off"` — no log at all

When the graph is rebuildable from source data, logging buys nothing. Examples are
a bulk load, a derived index, and a scratch analysis:

```python
g = kglite.open("kb.kgl", durable="off")
g.add_nodes(df, node_type="Topic", unique_id_field="id")   # nothing logged
g.save()          # one explicit checkpoint at the end
g.close()
```

This is **not** crash-safe. A snapshot is written only when *you* call
`save()`/`close()` or the context manager exits cleanly. If the process is killed
mid-session, the work since the last checkpoint is gone. That covers `kill -9`,
power loss, and an unhandled crash before the next `save()`.

### Taking a power-safe point on demand: `sync()`

`"normal"` skips the per-commit barrier, but you can take that barrier whenever
it matters:

```python
g = kglite.open("app.kgl", durable="normal")

def handle_request(payload):
    g.cypher("CREATE (:Event $props)", params={"props": payload})

handle_request(...)
g.sync()      # everything committed so far now survives power loss too
```

`sync()` writes **no checkpoint** and truncates nothing. It only makes the
existing log durable. That is the right granularity for "flush at the end of a
request" or "flush before shutdown". A full `save()` republishes the entire graph
and is far more expensive.

- Under `"full"`, `sync()` returns immediately, because every commit was already
  barriered.
- On a graph with no log, it raises `ValueError` rather than silently doing
  nothing. A caller who believes they bought power-safety and got nothing is the
  failure that costs data.

## How durability works

With the default `durable="full"`, every committed mutation is appended to a
`<path>-wal` sidecar and barriered to stable storage **before the call returns**.
`durable="normal"` writes the log without that per-commit barrier. Use `sync()`
when a power-safe point is required.

How it fits together:

- **Each mutation** → one WAL frame, written before the call returns.
  - Under `"full"` the frame is also barriered to stable storage per commit. That
    barrier adds device latency to the engine and WAL-encoding work (see "Cost
    and tuning" below).
  - Under `"normal"` the same frame is encoded and written but not barriered.
- **`save()`** → writes a full checkpoint (`.kgl`) and **truncates the WAL**. The
  checkpoint is the new baseline; the WAL starts empty again.
- **`open(...)`** → loads the last checkpoint, then **replays** any WAL frames
  written since it. This reconstructs the exact committed state, including work
  that was never checkpointed because the process crashed.

The on-disk state is therefore always "last checkpoint + replayable tail", and
reopen folds the two back together automatically.

### WAL format compatibility

The current writer uses WAL format **4**. The reader decodes formats **2–4**.

- **What format 4 records.** Complete node state, and the complete property-map
  multiplicity of each affected parallel-relationship group.
- **What changes.** The WAL sidecar, not the `.kgl` checkpoint format.
- **Upgrade.** A readable older WAL header is upgraded before new frames are
  appended.
- **Older readers.** Readers limited to formats 2/3 refuse the format 4 header,
  rather than interpreting unfamiliar frames as a torn tail.

Legacy formats 2/3 lack a discriminator for individual parallel relationships.

- If a surviving legacy edge action matches multiple checkpoint relationships,
  recovery refuses the ambiguity instead of choosing one.
- A later format 4 group snapshot can supply the complete final state.
- This does not reconstruct parallel members already lost by an earlier replay,
  or information absent from the old log.
- Unambiguous legacy operations remain readable.

Durable adoption also refuses duplicate exact `(primary type, id)` identities
before taking ownership or changing the WAL. This admission check does not change
the existing Cypher CREATE identity policy.

A surviving uncheckpointed frame that stores a raw legacy endpoint reference is
also refused. The refusal comes before replay, graph mutation, torn-tail repair,
or WAL truncation.

- Unlike a complete checkpoint, a WAL frame has no originating graph view from
  which a physical slot can be resolved safely.
- Frames at or below the checkpoint LSN are already represented by the complete
  snapshot and are skipped as residue.
- Move the refused sidecar aside only when deliberately discarding those
  uncheckpointed commits. Otherwise, recover with a compatible older build and
  write a clean checkpoint first.
- KGLite does not guess the former target.

### Crash recovery in practice

```python
import os

# Process A — commits, then dies hard before any save().
g = kglite.open("app.kgl", durable=True)
g.cypher("CREATE (:Person {id: 1, name: 'Alice'})")   # committed + fsync'd
g.cypher("CREATE (:Person {id: 2, name: 'Bob'})")     # committed + fsync'd
os._exit(1)   # hard crash — no save(), no clean close

# Process B — reopen recovers both, from the WAL.
g = kglite.open("app.kgl", durable=True)
assert g.cypher("MATCH (p:Person) RETURN count(p) AS n").scalar() == 2
g.save()   # checkpoint: fold the WAL into a fresh .kgl, truncate the log
```

Both rows survive the crash even though `save()` was never called in process A.
They were `fsync`'d to the WAL at commit time, and reopen replayed them.

## Choosing the mode

KGLite has five persistence postures for an embedded app. Pick by what you're
optimising for:

| You want… | Use | Trade-off |
|---|---|---|
| Every committed write to survive a hard crash | `open(path)` (the default) | One barrier per commit; reopen is O(graph) (loads the whole graph). |
| Committed writes to survive a crashing *process*, cheaply | `open(path, durable="normal")` | No barrier per commit; an OS crash or power cut loses work since the last `save()`. Call `sync()` for a power-safe point. |
| Maximum write throughput on rebuildable data | `open(path, durable="off")` | Nothing logged; a crash loses work since the last checkpoint. |
| Crash safety on a graph that outgrows RAM | `open(path, storage="mapped")` | Same per-commit WAL guarantee; property columns spill to mmap. The `.kgl` records the mode, so every later `open(path)` comes back mapped without repeating the argument — and passing `storage="mapped"` on a memory-saved graph converts it. See [Choosing a storage mode](../core-concepts.md#choosing-a-storage-mode). |
| 100 M+ nodes (Wikidata-scale), cheap cold-open | `open(path, storage="disk")` | Paged mmap, lazy load; **no per-commit WAL** — durability is your `save()` calls. |

The first three are **in-memory**: the whole graph lives in RAM, which is what
makes traversal and multi-hop queries fast. Durability adds crash-safety on top of
that model without changing the in-memory read path.

### If your app is growing

**If your app is simply growing, reach for `mapped`, not `disk`.** `mapped` is the
larger-than-RAM mode that keeps this guide's guarantee. It is durable by default,
and its crash recovery is kill-9 tested alongside in-memory.

`disk` is a different trade. It is a Wikidata-scale exploration mode whose commit
boundary is an explicit `save()`, not a logged write (see the Limitations below).
Choosing `disk` because a graph got big means giving up per-commit crash safety
you did not have to give up.

### If cold-open latency hurts

**If cold-open latency is what hurts, the storage mode is the lever, not the
log.**

- Reopening a `.kgl` decodes its whole payload before the first query answers, and
  that cost scales with the file.
- It is one serialized payload rather than an addressable layout, so there is
  nothing to defer.
- A `mapped` `.kgl` is read back by deserializing into a memory backend and then
  swapping it onto the mapped one. That is why a mapped reopen costs what a memory
  reopen costs.

`storage="disk"` is the only mode that changes that shape. A disk directory is
already in its query-ready layout, so opening it maps files instead of decoding a
payload.

- It is measured **6.5x faster to reopen at ~400k edges**.
- An external evaluation measured ~28x at 10.5M. Treat the small number as the
  floor rather than the rate.
- The price is the one above: no per-commit WAL, and durability is your `save()`
  calls.

Keeping the log and checkpointing regularly bounds *replay*, which is a different
cost from decoding the checkpoint. See [Cost and tuning](#cost-and-tuning).

## Serving concurrent reads

A `KnowledgeGraph` is single-owner. Don't share one instance across threads while
a thread mutates it; that raises a clear `RuntimeError`.

For a read-heavy server, take an immutable snapshot with `g.freeze()`. It returns
a `FrozenGraph` that shares the data via an O(1) clone and serves `cypher()` from
many threads at once, lock-free. When the data changes, build/reload, `freeze()`
again, and swap the snapshot in. See {doc}`/concepts/concurrency` for the full
model.

```python
snapshot = g.freeze()
# hand `snapshot` to N reader threads — concurrent, lock-free
snapshot.cypher("MATCH (o:Order) RETURN count(o)")
```

### Sessions on a durable graph

**Durability and shared concurrent writes don't combine in one handle.** A
`Session` (`graph.session()` / `kglite.open_session(...)`) serves shared reads and
serialized writes. Its `execute()` writes land on a working copy visible only
through that session, reachable by neither the log nor the owning graph's
`save()`.

- A `Session` write against a durable graph therefore **raises**, rather than
  applying a mutation nothing can persist. Reads are unaffected.
- For a durable app, keep writes on the durable `KnowledgeGraph` itself, where
  they are serialized and `fsync`'d. Use `freeze()` snapshots for concurrent reads.

After the source ends ownership, two cases apply:

- A retained Session without CDC may write to its private state. Those writes are
  not logged to the former source path.
- Sessions sharing a CDC stream remain read-only, because their independent
  mutations must not appear as changes to the unchanged source graph. Use the
  source KnowledgeGraph for captured writes.

Reach for Session writes without WAL/CDC when you need shared concurrent writes
but not persistence or capture authority. See {doc}`/concepts/concurrency` for the
full model.

## Cost and tuning

- **`"full"` adds a barrier per commit.** Device latency can dominate small
  writes, but it is only one component of their cost.
  - WAL encoding scales with the affected node state and the complete
    parallel-relationship groups sharing the same type, source and target.
  - Updating one member records the final property maps of every member in that
    group. Finding them also traverses adjacency.
  - Ordinary reads do not write or barrier the WAL.
- **`"normal"` skips only the barrier.** It retains the same state capture,
  encoding and log writes as `"full"`, including the cost of large parallel
  groups. It preserves process-crash recovery. `sync()` supplies a power-safe
  point when needed.
- **On macOS, `"full"` buys more than SQLite's default does.** KGLite's barrier is
  `F_FULLFSYNC`, which flushes the drive's own write cache. SQLite's default
  `synchronous=FULL` issues a plain `fsync`, which on macOS does not. The
  guarantees are therefore not the same thing measured differently. KGLite's
  default is the stronger one, and it costs accordingly.
- **Batch where you can.** One `cypher()` that creates 1,000 nodes is one
  `fsync`. 1,000 separate `cypher()` calls are 1,000 `fsync`s. Group related
  mutations into a single statement (or a transaction — see
  {doc}`/python/transactions`) when they logically commit together. Within a
  committed batch, each affected relationship group contributes one final
  snapshot, even when several mutations touched it.
- **Checkpoint to bound recovery time.** Reopen replays every WAL frame since the
  last `save()`. Replay is fast, because frames are folded into net per-entity
  state and the index rebuilt once. A periodic `save()` still keeps the WAL short
  and recovery near-instant for write-heavy, rarely-restarted services.

## Limitations

### Not available for `storage="disk"`

A disk graph commits by publishing an immutable generation, so its durability
boundary is that publish rather than a logical log. Reconciling a replayed frame
against a published generation needs a generation-aware log this release does not
have.

- `open(path, storage="disk")` therefore opens **non-durable**.
- **Both `durable="full"` and `durable="normal"` raise `ValueError`** rather than
  pretending. The levels are not uniform across storage modes, because the blocker
  here is the commit boundary itself and not barrier strength.
- `storage="disk"` supports only `durable="off"`.
- The in-memory default and `storage="mapped"` support every level. If you want
  crash safety on a graph that outgrew RAM, `mapped` is the answer.

What disk mode *does* guarantee is stronger than "no crash safety", and is kill-9
tested (`crates/kglite/tests/disk_crash_guarantee.rs`):

> A crash loses exactly the mutations made since the last `save()`, and
> nothing else. The graph reopens at the last published generation, complete
> and uncorrupted — never at a partially-written one.

- Between `save()` calls, disk-mode mutations live only in the process's heap
  overlay. Nothing is written, so nothing can be half-written.
- The publish itself is crash-atomic. The staged snapshot is `fsync`'d and
  renamed into place. Only then does an atomically-replaced `CURRENT` pointer
  select it, so a crash mid-publish leaves the previous generation selected.
- No acknowledged commit is ever lost. The acknowledgement point is your `save()`
  call.

#### Checkpoint cost

**Budget for the checkpoint's cost before you sprinkle `save()` calls.** Every
disk `save()` publishes a complete new generation, but untouched data is not
written again:

- **Untouched node types.** A node type nothing has touched since the previous
  generation is not written again. Its column file is hard-linked from the
  previous generation (a copy where links are unavailable, and always on
  Windows). Those bytes exist once on disk and cost no write time.
- **Index files.** The id and type index files are linked the same way while
  nothing has changed what they hold.
- **Lookup bundles.** The `title`/`nid` lookup bundles are carried instead of
  rebuilt until a title, an id or a new node has to be in them.
- **Still rewritten in full.** The CSR, node slots, edge properties and the files
  of any type you changed.

A checkpoint's cost therefore follows what you touched plus the size of the
graph's topology, not the size of the whole graph alone. {doc}`large-registers`
walks through a register of tens of millions of versions saved this way.

#### Generation pruning

Older generations are pruned for you. A save keeps the generation it just
published and one before it, and deletes the rest.

- A generation a reader in the same process still has mapped is kept until that
  reader is gone, and is removed by a later save. Examples are another handle
  loaded from the directory, a transaction, or a copy.
- A platform that refuses to delete a mapped file defers the deletion the same
  way.
- A failed deletion never fails the save.
- Set `KGLITE_KEEP_GENERATIONS` in the saving process to the number of *previous*
  generations to keep. `0` keeps only the current one. `all` keeps every
  generation and leaves pruning to you. The default is `1`.
- A reader in *another process* cannot be seen. One that stays more than that many
  generations behind the writer can lose the files it opens lazily. Raise the
  setting (or use `all`) when a long-lived reader shares a directory with a
  frequent writer.

#### Serving shape across checkpoints

A checkpoint keeps the graph's serving shape.

- **Stays memory-mapped.** A node type whose properties each hold values of one
  kind (integers, floats, booleans, text or dates), and whose titles are text,
  goes back into the memory-mapped column file. Reopening after the tenth save
  costs what reopening after the first did.
- **Saved to its own file and loaded into memory.** A type with a property holding
  several kinds (integers beside floats, say), a property holding other values
  (timestamps, points, lists), or non-text titles.
- **Earlier releases.** They moved every type of a reopened graph into such
  per-type files on its first re-save. That cost about six times the reload memory
  and made writes slower. The next save by this release restores the mapped
  layout.

### What the log carries

The log carries nodes, edges and labels, plus these:

- **Every declaration you make about them:**
  - the identity-field spellings an `add_nodes` call names (`unique_id_field` /
    `node_title_field`)
  - `set_parent_type`
  - `define_ontology` / `clear_ontology`
  - `create_index` / `drop_index` and their range and composite siblings
  - `CREATE CONSTRAINT` / `DROP CONSTRAINT`
  - `set_spatial`
  - `set_schema_version`
  - validity-interval declarations: `CALL db.temporal.declare` / `undeclare`,
    `set_temporal`, and the ones `validFrom`/`validTo` column types make
- **Two bulk payloads:**
  - **timeseries channels**: `set_timeseries`, `set_time_index`,
    `add_ts_channel`, `add_timeseries`, `add_nodes(timeseries=…)`
  - **embeddings**: `set_embeddings`, `add_embeddings`, `embed_texts`,
    `import_embeddings`, `copy_embeddings_from`, and the `build_vector_index`
    declaration

A recovered graph is queryable by your own column names, with your indexes built,
your constraints enforced, your series readable and your vectors searchable.
Embeddings carry the model id and per-node text hashes, so a following
`embed_texts(mode='changed')` re-embeds nothing.

Replay does *not* restore derived state a load rebuilds anyway. The HNSW topology
is rebuilt from the replayed vectors rather than logged, because it addresses
store slots that replay renumbers.

### Bulk payloads make bulk frames

A timeseries load logs the series it produced. A 10 000-node × 365-day ×
3-channel `add_timeseries` writes one frame of roughly 129 MB, assembled in
memory before a single write.

That is ~3.6× cheaper per source row than the node rows the log already carries,
and far inside the 4 GiB frame cap. It is still a real transient cost on a
Wikidata-scale ingest. `durable="off"` (or loading before `open(…,
durable=…)`) skips it.

### A `with` block is not a transaction

Each mutation commits as it runs. An exception inside the block does not undo
mutations that already returned. They are recovered on the next `open()`. Use
`begin()` when you want discard-on-error ({doc}`/python/transactions`).

## See also

- {doc}`/python/transactions` — `begin()` / `commit()` / `rollback()`,
  snapshot isolation, and how the Bolt server consumes the same surface.
- {doc}`/python/core-concepts` — the memory / mapped / disk storage modes.
- {doc}`data-loading` — bulk-loading the seed data an app starts from.
