# Storage and memory

A loaded graph keeps its topology and indexes on the heap and can serve its large vectors and columns straight from the `.kgl` file. This guide says what lives where, how to change it, and how to read the memory numbers.

## What lives where

Placement follows the data, not the storage mode. The mode only decides the default for columns.

| Data | Where it lives | Notes |
|---|---|---|
| Topology (nodes, relationships) | Heap in `memory` and `mapped`; mmap CSR in `disk` | 40 bytes per node and 32 per relationship on the heap |
| Indexes (type, property, id, HNSW structure) | Heap | Built or rebuilt at load |
| Small columns (under 256 KB) | Heap | Stored inside the compressed sections |
| Large columns (256 KB or more) | Heap, spill file or the `.kgl` file | See `columns=` below |
| Embedding stores of 64 MB or more | The `.kgl` file (`embeddings="auto"`) | Read in place, no copy |
| Smaller embedding stores | Heap | `embeddings="mapped"` maps any size |

A mapped region is read in place from the file. It costs no resident memory until a query touches its pages.

## The knobs

Four options control placement and the file layout. `kglite.load()` takes all four. `open_session(...)` (read-only form) takes the first three. The MCP server (`--embeddings`, `--columns`) and `kglite query` take `embeddings` and `columns`.

| Option | Values | Default | Effect |
|---|---|---|---|
| `embeddings=` | `"auto"`, `"mapped"`, `"memory"` | `"auto"` | Where embedding stores live |
| `columns=` | `"auto"`, `"mapped"`, `"memory"` | `"memory"` in memory mode; `"auto"` in `mapped` mode and the MCP server | Where large property columns live |
| `verify=` | `"lazy"`, `"eager"` | `"lazy"` | When section checksums run |
| `layout=` (on `save()`) | `"auto"`, `"mapped"`, `"compact"` | `"auto"` | Which container version is written, and whether columns are mappable |

```python
g = kglite.load("graph.kgl")                       # defaults
g = kglite.load("graph.kgl", columns="auto")       # also serve large columns from the file
g = kglite.load("graph.kgl", embeddings="memory")  # copy vectors to the heap
g.save("graph.kgl", layout="mapped")               # also store large columns raw, mappable
g.save("graph.kgl", layout="compact")              # v8 file older kglite can read
report = g.verify_integrity()                      # {"ok": bool, "sections": [...]}
```

Meanings of the values:

- `auto` maps a store of 64 MB or more (columns: 256 KB or more) from a regular local file on Unix.
- `mapped` maps every store or column of a v9 file, whatever its size.
- `memory` copies to the heap. Columns of 256 KB or more spill to a temp file, as in earlier releases.
- `lazy` checks the CRC32 of a section that stays mapped at its first use: a search, a save, `verify_integrity()` or the write that promotes it to the heap. A section copied to the heap at load is always checked at load, as are string columns, which are also UTF-8-validated.
- `eager` checks every section at open. `KGLITE_VERIFY=eager` sets it for a process.

`columns=` only has an effect on a file saved with `layout="mapped"`. If you do not pass it, an in-memory graph decodes its columns. A scan over mapped column pages measured 4-9 % slower than over decoded ones (50k rows, release build), and in-memory speed is the core product. `Auto` stays the default for `mapped` storage and the MCP server.

## Container v9 and v8

A default `save()` writes v9 when the graph holds an embedding store of 4 MiB or more. Otherwise it is v8, with the same bytes as before. `layout="mapped"` also writes columns of 256 KB or more raw, so it writes v9 for those too.

- A v9 file stores the vectors and their norms (with `layout="mapped"`, also the large columns) uncompressed and 64-byte aligned, after the compressed sections.
- Embeddings compress by under 10 %, so the file grows little. Columns that do not delta-compress (floats, strings, hashed ids) grow 1.4-5.4x on the operator graphs measured (one 134 MB graph became 395 MB); zstd shrinks their text 2.9-7.2x.
- Columns are not mappable by default because a mapped string column does not save memory. The load validates UTF-8 and the checksum over every string page, which makes them resident, and the file is several times larger. Pass `layout="mapped"` when the file is far larger than the memory you want resident and the columns are mostly numeric.
- Integer columns that shrink four times or more as deltas (monotonic ids) stay compressed and decode at load.
- A kglite release before this one refuses a v9 file: "File uses .kgl container version 9, but this library only supports up to version 8. Please upgrade kglite."
- If a consumer must stay on an older kglite, save with `layout="compact"`. It writes v8 with every column and vector compressed.
- kglite reads v8 and older files unchanged. `embeddings=` and `columns=` have no effect on them.

## Footprint and RSS

The two metrics disagree about mapped files, so name the one you quote.

| Metric | Counts mapped file pages? |
|---|---|
| macOS `phys_footprint` (`footprint <pid>`, Activity Monitor "Memory") | No, while the pages are clean |
| Linux RSS, cgroup memory | Yes, for pages a query touched |

- Clean file-backed pages are reclaimable under memory pressure and shared by every process that maps the same file.
- Mapped pages are therefore cheaper than heap pages on both platforms, but only macOS footprint shows it as zero.
- Measured on a 205 MB embedding store (50k vectors x 1024, macOS, release build): load 334 to 54 ms, and footprint after load 242 to 44 MB (76 MB after queries).
- The first brute-force query after load costs about 13 ms more (15.5 against 2.5 ms), because it faults the pages in and checks the CRC. Warm queries match the heap.
- A `layout="mapped"` file's string columns are checked at load, which reads every page. The load drops those pages afterwards, so the resident set follows the rows queries read: the MCP server on a 395 MB file went from 656 to 330 MB RSS after boot. Linux uses `madvise(MADV_DONTNEED)` and macOS re-maps the range, which keeps the file in the OS cache so the next boot stays warm; the Linux path was not measured.
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
