# Storage and memory

A graph loaded into `mapped` storage serves its topology, indexes, large vectors and columns straight from the `.kgl` file. A graph loaded into `memory` storage keeps the topology and indexes on the heap and serves only its large vectors and columns from the file. This guide says what lives where, how to change it, and how to read the memory numbers.

## What lives where

Placement follows the data, not the storage mode.

| Data | Where it lives | Notes |
|---|---|---|
| Topology (nodes, relationships) | The `.kgl` file in `mapped` storage; heap in `memory`; mmap CSR in `disk` | A save records it for a graph of 65,536 nodes or 262,144 relationships or more. On the heap it costs 40 bytes per node and 32 per relationship |
| Node type member lists and the id index of an all-`UniqueId`, all-int64 or all-string type | The `.kgl` file in `mapped` storage; heap in `memory` | A save records them for a graph of 65,536 nodes or more |
| Peer counts per relationship type | The `.kgl` file in `mapped` storage; built on first use in `memory` | A save records them for 262,144 relationships or more |
| Other indexes (property, string-id, HNSW structure) | Heap | Built or rebuilt at load |
| Small columns (under 256 KB) | Heap | Stored inside the compressed sections |
| Large columns (a buffer of 256 KB or more) | The `.kgl` file (`columns="auto"`) | Read in place, no spill file; `columns="memory"` spills them |
| Embedding stores of 64 MB or more | The `.kgl` file (`embeddings="auto"`) | Read in place, no copy |
| Smaller embedding stores | Heap | `embeddings="mapped"` maps any size |

A mapped region is read in place from the file. It costs no resident memory until a query touches its pages. A write to a `mapped` graph that serves its topology from the file puts an overlay over it, so the heap holds the writes and not the topology. Past a quarter of the graph's nodes plus relationships the overlay collapses into a heap topology, once; from then on the graph behaves like any heap-topology `mapped` graph.

## The knobs

Five options control placement and the file layout. `kglite.load()` takes all five. `open_session(...)` (read-only form) takes the first four. The MCP server (`--embeddings`, `--columns`, `--topology`) and `kglite query` take `embeddings`, `columns` and `topology`. The C ABI's `KgliteLoadOptions` has the same three fields.

| Option | Values | Default | Effect |
|---|---|---|---|
| `embeddings=` | `"auto"`, `"mapped"`, `"memory"` | `"auto"` | Where embedding stores live |
| `columns=` | `"auto"`, `"mapped"`, `"memory"` | `"auto"` | Where large property columns live |
| `topology=` | `"auto"`, `"mapped"`, `"memory"` | `"auto"` | Whether a `mapped` load reads the topology and index sections in place |
| `verify=` | `"lazy"`, `"eager"` | `"lazy"` | When section checksums run |
| `layout=` (on `save()`) | `"auto"`, `"compact"` | `"auto"` | Which container version is written, and whether large buffers are stored raw |

```python
g = kglite.load("graph.kgl")                       # defaults
g = kglite.load("graph.kgl", columns="memory")     # decode columns; large ones spill to a temp file
g = kglite.load("graph.kgl", embeddings="memory")  # copy vectors to the heap
g = kglite.load("graph.kgl", topology="memory")    # decode the topology, rebuild the indexes
g.save("graph.kgl", layout="compact")              # v8 file older kglite can read
report = g.verify_integrity()                      # {"ok": bool, "sections": [...]}
```

Meanings of the values:

- `auto` maps a store of 64 MB or more (columns: 256 KB or more) from a regular local file on Unix.
- `mapped` maps every store or column of a v9 file, whatever its size.
- `memory` copies to the heap. Columns of 256 KB or more spill to a temp file, as in earlier releases.
- For `topology=`, `auto` and `mapped` read the sections in place (Windows copies them) and `memory` ignores them: the compressed topology is decoded and the indexes are rebuilt, as before v9 recorded them. The graph still continues in `mapped` storage.
- `lazy` checks the CRC32 of a section that stays mapped at its first use: a search, a save, `verify_integrity()` or the write that promotes it to the heap. A section copied to the heap at load is always checked at load, as are string columns, which are also UTF-8-validated. A numeric column that stays mapped is therefore not checked at load; pass `verify="eager"` to check it.
- `eager` checks every section at open. `KGLITE_VERIFY=eager` sets it for a process.

`columns=` and `topology=` only have an effect on a v9 file. A v8 file, and one saved with `layout="compact"`, has nothing raw to map. `topology=` also has no effect on a graph that loads into `memory` storage: it never reads the sections. A file without topology sections loads as it does under `auto`, whatever `topology=` says.

## Container v9 and v8

A default `save()` writes v9 when any of these holds. An embedding store of 4 MiB or more. A column with a buffer of 256 KB or more. 65,536 nodes or more. 262,144 relationships or more. Otherwise it is v8, with the same bytes as before.

- The rule for "large" is the one a v8 load uses to spill a column to a temp file. A default file therefore maps exactly the columns that would otherwise be copied into a per-process spill file. Many servers on one graph share the file's pages instead of each writing its own copy under `$TMPDIR`.
- A v9 file stores the vectors, their norms and those column buffers uncompressed and 64-byte aligned, after the compressed sections. Smaller columns stay in the compressed sections and decode to the heap.
- Embeddings compress by under 10 %, so they add little. Raw column text does not compress away. On the operator graphs measured the file grows 1.4-5.4x (one 134 MB graph becomes 389 MB). Almost all of the growth is string bytes that zstd shrinks 3-7x.
- Integer columns that shrink four times or more as deltas (monotonic ids) stay compressed and decode at load.
- A graph of 65,536 nodes or more also records the sorted member list of each node type. A type whose ids are all `UniqueId`, all unique int64 or all unique strings also gets a sorted id index. A graph of 262,144 relationships or more records the per-relationship-type peer counts. Either size also records the topology raw, at about 40 bytes per node and 32 per relationship, on top of the compressed copy.
- These sections cost 4 bytes per node, 8 per indexed id and 16 per distinct peer. Measured: a random 1M-node, 12M-relationship graph grows 110 to 147 MB; a 196 MB operator graph grows 1.3 % and a 134 MB one 3.7 %.
- A kglite release before this one refuses a v9 file: "File uses .kgl container version 9, but this library only supports up to version 8. Please upgrade kglite."
- If a consumer must stay on an older kglite, or the file size matters more than a spill file, save with `layout="compact"`. It writes v8 with every column and vector compressed.
- kglite reads v8 and older files unchanged. `embeddings=` and `columns=` have no effect on them.

## Topology served from the file

A save of a graph with 65,536 nodes or 262,144 relationships or more records the topology raw in the data region. A load into `mapped` storage reads it in place instead of decoding the compressed copy.

- The file holds one 32-byte record per node slot and one endpoint record per relationship slot. It also holds an out- and an in-adjacency sorted by relationship type, so a typed expansion binary-searches its node's run. Node and relationship slots are the ones the graph held when it was saved, deleted slots included.
- The heap holds only the per-type templates, the sparse map of relationships that carry properties, and what the declared indexes and small columns need. Relationship properties are still decoded to that map at load.
- A write puts an overlay over the file's topology instead of copying it. The heap grows with the writes. The overlay collapses into a heap topology, once, when it passes a quarter of the graph's nodes plus relationships or on a whole-graph operation such as `vacuum()`. A copy, a snapshot or a transaction that did not write keeps sharing the file's topology. A failed statement rolls back to the slots it vacated.
- A transaction copies the overlay's writes when it starts: 43 µs at 1,000 writes and 0.66 ms at 100,000, against 22 µs in `memory` storage (1M nodes, 12M relationships).
- A save copies the topology out of the file and the overlay's writes with it, so it needs a transient heap graph the size of the topology. The overlay keeps serving afterwards; a reload of the saved file starts from an empty overlay.
- The order a node's relationships come back in can differ from a `memory` load. Relationships of one type keep memory storage's newest-first order. Those of different types are grouped by type.
- A save skips the sections in four cases. The graph is below the floors. It holds a node with an inline identity. It has more than 262,144 relationships with properties. It has more than 1,024 node or relationship types. `layout="compact"` writes none. A file without them loads as before.
- A damaged node section refuses a `mapped` load. The other topology sections are checked at first use: `verify_integrity()`, a save, and the collapse of a write overlay refuse a damaged section.
- `topology="memory"` ignores the sections. `KGLITE_MMAP=0` copies them to the heap instead of mapping them, and the graph still serves from the copies.
- `layout="compact"` writes the v8 file with the compressed topology and no raw sections.

### Reading the memory numbers

Mapped topology changes two numbers differently.

| Number | Behaviour with a mapped topology |
|---|---|
| Private memory (macOS `phys_footprint`) | Stays roughly constant plus the data the session writes |
| Resident memory (Linux RSS, cgroups) | Follows the working set the queries touch, which is clean and reclaimable |

`graph_info()` reports the split:

- `topology_mapped_bytes`: bytes read in place from the file. `0` for a heap topology, `topology="memory"` and `KGLITE_MMAP=0`.
- `topology_heap_bytes`: the node and edge arrays of a heap graph (40 and 32 bytes per slot). For a mapped topology, only what the load copied or decoded.
- `topology_overlay_entries`: writes held by the overlay. They are heap, and `0` before the first write.

## Indexes served from the file

A load into `mapped` storage serves the recorded indexes from the file. A load into `memory` storage ignores them and builds what it always built.

- The first `{id: ...}` lookup on a recorded type costs nothing to build and holds no heap: a binary search in the mapping. Before, it built a hash map of every id (1M ids: 23 ms and +83 MB, now 0.2 ms).
- The first grouped aggregate over a relationship type decodes one direction's counts from the file instead of scanning every relationship (1M nodes, 12M relationships, `b.id, count(a)` over one type: 590 ms and +261 MB before, 235 ms and +179 MB after; `memory` storage takes 560 ms and +227 MB).
- A type with repeated ids, or ids of mixed kinds, has no recorded id index and builds one on first use, as before.
- A write to a node type layers over its recorded member list and id index. The first relationship write drops the recorded peer counts, which are then built from the relationships on first use.
- `topology="memory"` ignores them and builds what a `memory` load builds. `KGLITE_MMAP=0` copies them to the heap instead of mapping them; the load still skips the scan.
- A damaged index section refuses a `mapped` load, because the load reads and checks every one.

## Footprint and RSS

The two metrics disagree about mapped files, so name the one you quote.

| Metric | Counts mapped file pages? |
|---|---|
| macOS `phys_footprint` (`footprint <pid>`, Activity Monitor "Memory") | No, while the pages are clean |
| Linux RSS, cgroup memory | Yes, for pages a query touched |

- Clean file-backed pages are reclaimable under memory pressure and shared by every process that maps the same file.
- Mapped pages are therefore cheaper than heap pages on both platforms, but only macOS footprint shows it as zero.
- Measured on a 205 MB embedding store (50k vectors x 1024, macOS, release build): load 334 to 54 ms. Footprint after load 242 to 44 MB (76 MB after queries).
- The first brute-force query after load costs about 13 ms more (15.5 against 2.5 ms), because it faults the pages in and checks the CRC. Warm queries match the heap.
- A mapped string column is checked at load, which reads every page. The load drops those pages afterwards, so the resident set follows the rows queries read. The MCP server on a 389 MB file went from 656 to 330 MB RSS after boot. Linux uses `madvise(MADV_DONTNEED)` and macOS re-maps the range, which keeps the file in the OS cache so the next boot stays warm; the Linux path was not measured.
- Linux RSS and PSS were not measured for this release.

### Memory that stays charged after a free

Freed allocator pages return to the OS only when the allocator is told to collect. `load()`, `save()` and the embedding writers (`import_embeddings`, `remove_embeddings`, `set_embeddings`, `embed_texts`, `copy_embeddings_from`) do that for you. For other seams call `kglite.trim_memory()`.

The MCP server started from the wheel (`kglite-mcp-server`) releases memory itself after every graph load, reload and rebuild. Measured with `phys_footprint` after boot and queries: a 195 MB graph fell from 700-732 MB to 229 MB, and a 134 MB graph from 187-231 MB to 138 MB. A standalone `kglite-mcp-server` binary uses the system allocator and has nothing to release.

If you spawn a subprocess between freeing a store and the trim, the child holds a copy-on-write view of the parent's heap and pins those pages. A removed 50k x 1024 store then stays at 212 MB instead of falling to 17 MB. Trim first, then spawn.

## Overwriting a served file

A running process that maps a `.kgl` file must never see the file truncated in place.

- Replace a served file by writing a new file beside it and renaming it over the old one. The old inode stays readable until the last mapping drops.
- `save()` already does this: temp file, fsync, rename.
- `cp`, `rsync --inplace` and `>` redirection truncate the live file. A running server then faults with SIGBUS on its next touch of a mapped page.
- Saving over the path a graph is mapped from is safe on Unix. On Windows the graph copies its mapped stores to the heap first.
- `g.verify_integrity()` reports a source file whose modification time changed since it was mapped.

## First write to a mapped buffer

The first write to a mapped embedding store, or to a mapped column, copies that store or column to the heap. Other stores and columns stay mapped.

- Clones, snapshots and transactions that did not write keep the mapping.
- In `mapped` storage mode the existing spill-after-write moves a promoted column back to a file.

## Platforms and fallbacks

`auto` maps only where mapping is safe. These cases load to the heap:

- Windows, under `auto`.
- Byte buffers (`load_kgl_bytes`).
- Non-regular files, network filesystems and FUSE mounts.
- Big-endian hosts.
- `KGLITE_MMAP=0` forces the heap everywhere.

## Storage modes

`mapped` storage and `disk` storage are separate addons for graphs that do not fit on the heap; see {doc}`/python/core-concepts`. Disk graphs do not carry quantized embedding copies and write no v9 sections. Quantization is described in {doc}`embedding-quantization`.
