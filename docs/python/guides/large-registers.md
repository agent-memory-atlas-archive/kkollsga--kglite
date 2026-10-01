# Large registers on disk

A *register* is a history table that only grows at one end: every row is a
version of something, a delivery appends the day's new versions and closes the
ones they replace, and nobody rewrites the past. An HR system's employment
history is one, a price list or a product catalogue's revision log is another.
This page is for the person who holds such a register at tens of millions of
versions on one machine and wants to know how to build it, how to keep it
current, and what each step costs.

The examples use one synthetic employment register: an `Employment` node is one
version of one person's position, valid from one date until the next version
begins, and a `Person` node is the anchor the versions hang from. At the scale
described here, "a 25-million-version register", the point is that none of the
steps below needs the whole register in memory: building is chunked, reopening
maps files, an appended delivery costs what the delivery weighs, and a save
writes the types the delivery touched and the graph's topology and links the
rest (section 4 is honest about what that still costs). The numbers on this page were measured on one machine (an Apple
M4 with 16 GB, a release build, run on 30 September and 1 October 2026 with
other work running on it) and are the envelope of that machine, not a promise
for yours.

Disk mode has no write-ahead log: your `save()` calls are the durability
boundary ({doc}`durable-apps`). Everything below assumes you accept that. Valid
time and the declarations used here are described in {doc}`valid-time`; the
bitemporal modelling of a register that records corrections is in
{doc}`bitemporal`.

## 1. Build in chunks, saving after each

Create the graph in disk storage from the start and load it in chunks of a few
hundred thousand versions, saving after each chunk. Nothing in the process has
to hold the whole register: after each save the graph serves its files from
disk, so the heap holds the chunk in flight and what the save needs, not what
has been loaded so far.

```python
import kglite

g = kglite.open("employment.kgl", storage="disk")   # a new directory on first save

for i, part in enumerate(chunks()):                 # e.g. 500,000 versions each
    g.add_nodes(part.versions, "Employment", "id", "person_no")
    if i == 0:
        g.set_temporal("Employment", "valid_from", "valid_to", convention="half_open")
    g.add_nodes(part.people, "Person", "id", conflict_handling="skip")
    g.add_relationships(part.edges, "OF", "Employment", "id", "Person", "person_no")
    g.save()
```

Give each version an integer `id` and keep ids that do not fit 32 bits (a
register number, a 12-digit key) as integers: a type whose ids are all integers
persists its id index as a sorted array searched in the mapping, 12 bytes an id,
and a datetime property is a typed column of microseconds, 8 bytes and a null
byte a row. A pandas column of integers with a blank is `float64`; a
whole-number float id is read as an integer, and one that is not a whole number
raises. Declare the validity interval once, on the first chunk, as the example does.

The register was built this way in one process: 24.7 million versions in 25
chunks, each chunk's load taking 3 to 5
seconds whatever the size so far, the footprint peaking at about 3.5 GB during
a save and settling below 1 GB after it. Every chunk changes the version type,
so each save rewrites its file: the saves grow with the register, the last of
them taking a few minutes.

## 2. Reopen

```python
g = kglite.open("employment.kgl", storage="disk")   # or kglite.load("employment.kgl")
```

A disk directory is already in its query-ready layout, so opening it maps
files rather than reading rows: the 24.7-million-version register reopened with
the process footprint growing by about 3 MB (the open itself took about 15 s,
which goes to validating the index and column files). Queries then page in what
they touch. `FOR VALID_TIME AS OF` and the rest of the Cypher surface work as
on a graph in memory.

## 3. Apply a delivery

A delivery is an append and some closes. Both leave the register's files
alone and touch only the rows involved.

```python
g.add_nodes(delivery.versions, "Employment", "id", "person_no")        # new versions
g.cypher("UNWIND $ids AS i MATCH (e:Employment {id: i}) "
         "SET e.valid_to = datetime($day)", params={"ids": closed_ids, "day": day})
```

- **Appending** rows (`add_nodes`, `CREATE`, `MERGE`) to a type the reopened
  graph serves from its file costs the new rows: they go into a small tail store
  beside the mapped file, and the type's index buckets are layered over their
  files. A 1,000-row append to the 24.7-million-version register took 0.02 s and
  left 0.7 MB on the heap (27 s and 185 MB before the tail store).
- **The first `SET` of a column** of a type served from its file copies that
  column onto the heap once: about 0.3 to 0.5 s and 360 MB for a text column at
  24.7 million rows, a timestamp column a little more. Later statements on that
  column are per-row work, 1 to 2 µs a row. Close your versions with one
  statement per delivery, not one per row.
- **Deleting** rows a statement created moments ago, and statement rollback, are
  proportional to the rows involved, not to the type.

Nothing is durable yet. A crash here loses the delivery and nothing else; the
last published generation reopens intact.

## 4. Publish with `save()`

`save()` writes a new, complete generation of the directory and then switches
the `CURRENT` pointer to it; a reader that opened the previous generation keeps
reading it. What the save has to write, and what it reuses from the generation
it replaces:

| Part of the generation | Nothing changed | After a `SET` | After an append |
|---|---|---|---|
| Column file of a type you did not touch | linked from the previous generation | linked | linked |
| Column file of the type you changed | linked | rewritten: the old file's cells with the changed ones laid over them | rewritten: the file's regions, then the new rows |
| `id_indices.bin`, `type_indices.bin` | linked | linked (a `SET` moves no id or membership) | rewritten |
| Global `title` / `nid` lookup bundles | carried | carried unless the statement wrote a title or an id | rebuilt (one pass over every node) |
| CSR, node slots, edge properties | rewritten | rewritten | rewritten |

A link is a hard link where the file system allows one (a copy where it does
not, and always on Windows): the bytes exist once on disk and cost no write
time. Published files are never written to again, which is what lets two
generations share one.

The cost of a save is therefore what you touched plus the size of the graph's
topology, not the size of the whole graph: the CSR and the node slots are
rewritten by every save, and the file of a type that a delivery touches (every
delivery touches the version type) is rewritten whole.

On the 24.7-million-version register (release build, 16 GB Apple M4, other work
loading the machine at an average of 3 to 14 and then 18 to 30 for the second
run, 1 October 2026; wall time of a `save()`, best of two runs, with the time
before the index files were linked and the lookup bundles carried in
parentheses):

| What changed since the last save | Save | Footprint above the start |
|---|---|---|
| nothing | 29 s (49 s) | 2 MB (199 MB) |
| a `SET` of the small anchor type only | 23 s (47 s) | 132 MB (317 MB) |
| a `SET` of 1,000 `status` values on the version type | 67 s (96 s) | 950 MB |
| an append of 1,000 rows | 122 s (137 s) | 1.8 GB |

What is left in a save is the CSR and the node slots (22 to 34 s every time,
whatever changed), the touched type's file (3.6 GB: 40 s for the `SET`, 47 s for
the append) and, after an append, the lookup bundles and the compaction of the
new edges (38 s). On an 8-million-version register the same four saves took 2.7,
3.4, 5.2 and 8.7 s (best of three; 4.7, 5.0, 7.0 and 12.1 s before).

A delivery that touches the same type every day pays for that type's file every
day. If that is the cost you cannot carry, shard the register by time or by key
range into several types or several graphs, so that a delivery touches one
small one.

## 5. Generations and retention

Every save publishes a generation, so a directory would grow by a whole copy per
save. A save therefore deletes the generations older than the one before the
generation it just published. Set `KGLITE_KEEP_GENERATIONS` in the saving
process to the number of previous generations to keep (the default is `1`, `0`
keeps only the current one, `all` keeps every generation and leaves pruning to
you). A generation that something in the same process still maps (a second
handle, a transaction, a copy) waits until that reader is gone, and a platform
that refuses to delete a mapped file defers the deletion to a later save; a
failed deletion never fails the save.

The files two generations share survive the older generation's deletion: a
deleted name is an unlinked directory entry, and the file lives on while another
generation names it. With the default window the directory holds two
generations, and the disk cost of the older one is the files the newer one did
not carry over.

## 6. Readers in other processes

A reader in another process cannot be seen by the writer, so it takes the
consequences of the writer's pruning:

- Files a reader has **mapped** stay valid on POSIX systems when the writer
  deletes their generation: the mapping outlives the name. (Windows defers the
  deletion in the writer instead.)
- Files a reader opens **lazily**, after the writer has pruned the generation it
  is on, are gone. The lookup bundles are such files: a bundle that cannot be
  opened any more declines to a scan, so the answer stays correct and gets slower.
- A reader that stays more than the kept number of generations behind the writer
  should therefore be given a larger `KGLITE_KEEP_GENERATIONS` (or `all`), or
  reopen on a schedule.

## 7. What this is not

- **Not a write-ahead log.** A crash loses every mutation since the last
  `save()`, and nothing else; between saves nothing is written.
- **Not a database that answers while it writes.** One process holds the writer
  lease; a second writer on the directory waits for it or is refused.
- **Not in-memory speed.** In-memory graphs are faster, and the disk modes exist
  for registers that do not fit. Choose the storage mode by what fits: see
  {doc}`/python/core-concepts`.
