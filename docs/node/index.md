# Node.js guide

`kglite-node` embeds the KGLite engine in a Node.js process. It is a native addon built with napi-rs over `kglite::api`, so it is a Rust-side wrapper and shares the engine, the Cypher dialect and the `.kgl` file format with the Python wheel and the Bolt server.

## Install

Run `npm install kglite-node`. The package ships prebuilt binaries, so you need no Rust toolchain, compiler or `node-gyp`.

| Platform | Notes |
|---|---|
| macOS arm64, x64 | |
| Linux x64, arm64 (glibc) | Requires glibc 2.35 or newer |
| Linux x64, arm64 (musl) | Alpine and similar |
| Windows x64 | MSVC build |

Node 20 or newer is required.

## Quick start

```js
const { open } = require('kglite-node');

const graph = await open('./people.kgl');
await graph.executeWrite(
  'CREATE (:Person {name: $name, born: $born})',
  { name: 'Ada', born: 1815 },
);
const { rows } = await graph.executeRead(
  'MATCH (p:Person) WHERE p.born < $year RETURN p.name AS name',
  { year: 1900 },
);
console.log(rows); // [ { name: 'Ada' } ]
await graph.close();
```

Every method returns a promise. `Graph` implements `Symbol.asyncDispose`, so `await using graph = await open(path)` closes it for you. A `Transaction` does the same and rolls back.

For the query language itself, see the [Cypher reference](../reference/cypher-reference.md).

## Opening a graph

`open(path, options?)` opens or creates the graph at `path` and returns a `Graph`.

| Option | Default | Meaning |
|---|---|---|
| `durability` | `'full'` | Crash-safety level (below). Refused with `storage: 'disk'` when set explicitly. |
| `storage` | engine default | `'memory'`, `'mapped'` or `'disk'`. |
| `readOnly` | `false` | Load the last checkpoint and take no lease. Writes reject with `ReadOnly`. Not combinable with `durability`, `storage`, `lockTimeoutMs` or `autoCheckpointWalMib`. |
| `lockTimeoutMs` | `0` | How long to wait for another writer to release the path. `0` fails fast. |
| `autoCheckpointWalMib` | `16` | Write-ahead log size past which a commit starts a background online checkpoint. Writers keep committing; it never runs on the JavaScript thread. `0` disables it. Not combinable with `readOnly`. |
| `timeoutMs` | `180000` | Per-query deadline. `0` disables it. |
| `rowLimit` | none | Per-query cap on returned rows. |
| `integers` | `'safe'` | `'bigint'` returns every integer as a `bigint`. |
| `validTimeDefault` | `'today'` | Instant an unprefixed statement reads on a graph with declared validity intervals: `'today'`, `'all'` (no valid-time filtering) or a `'YYYY-MM-DD'` day. Runtime only; not saved. Any other value rejects `InvalidArgument`. |

Opening quarantines or repairs a damaged write-ahead log and can degrade the durability level. Each case adds an entry to `graph.openInfo.advisories`.

`graph.path`, `graph.durability`, `graph.readOnly` and `graph.closed` report the handle's state. `graph.openInfo` reports what the open did:

| Field | Value |
|---|---|
| `path`, `readOnly`, `created` | The opened path, whether writes are refused, and whether the open created the graph. |
| `storage`, `durability` | The storage mode and durability level now in force. |
| `degradedFrom` | The requested level that degraded to `'off'`. Absent when nothing degraded. |
| `convertedFrom` | The mode an explicit `storage` converted the graph from. Absent when nothing converted. |
| `advisories` | `{ code, message, affected }` entries, such as `wal_quarantined` or `wal_tail_saved`. Empty when the open was clean. |

### Durability levels

| Level | Behavior |
|---|---|
| `'full'` | Every commit is fsynced to the write-ahead log before it resolves. |
| `'normal'` | Commits reach the log but are fsynced in batches. Call `graph.sync()` for a power-safe point. |
| `'off'` | No write-ahead log. Changes persist only at a checkpoint. `sync()` rejects with `NotDurable`. |

A `readOnly` graph reports `'off'`. A disk graph degrades an inherited level to `'off'` and reports it as `openInfo.degradedFrom`.

### Checkpoint and close

- `graph.checkpoint()` folds the write-ahead log into the `.kgl` file. It resolves `{ written, version }`: `written` is `false`, and no file is written, when nothing changed since this handle's last checkpoint. `version` is a `number`, or a `bigint` beyond 2^53 - 1.
- `graph.close()` checkpoints a writable graph that has unsaved changes, releases the writer lease and closes the handle. It is idempotent.
- `close()` rolls back any transaction still open on the graph.
- After `close()`, every method rejects with `Closed`.

A process that exits without `close()` loses no committed work at `'full'`. The next `open()` replays the log.

## Queries

`executeRead` runs a read-only statement. `executeWrite` runs a statement that may write, as one auto-committed transaction. A mutating statement given to `executeRead` rejects with `InvalidArgument`.

Both take `(cypher, params?, options?)` and resolve to a `QueryResult`:

| Field | Meaning |
|---|---|
| `columns` | Column names in order. |
| `rows` | One object per row, keyed by column name. |
| `stats` | Mutation counters. Present for statements that write. |
| `warnings` | Advisory warnings for this query. They are never printed. |
| `truncated` | `{ rowLimit, totalRows }`. Present only when `rowLimit` dropped rows. |

Reference parameters as `$name`. `QueryOptions` overrides per call:

| Option | Meaning |
|---|---|
| `timeoutMs` | Deadline in milliseconds. `0` disables it. |
| `rowLimit` | Cap on returned rows. |
| `maxWorkUnits` | Work budget, not a row cap. Exceeding it fails the query. |

## Value mapping

### Parameters

| JavaScript | Cypher |
|---|---|
| `null` | `NULL` |
| `boolean`, `string` | Boolean, string |
| `number` that is a safe integer | Integer |
| any other `number` | Float |
| `bigint` | Integer. Outside the signed 64-bit range it rejects. |
| `KgFloat` | Float, even for `1` |
| `Date` | UTC datetime |
| `LocalDate`, `LocalDateTime`, `Duration`, `Point` | The matching Cypher type |
| `Array`, plain object | List, map |

`NaN`, `Infinity`, `-Infinity` and `-0` are sent as floats unchanged.

A JavaScript `1` is an integer because JavaScript cannot tell `1` from `1.0`. Wrap it in `new KgFloat(1)` to send a float. `undefined` map entries are omitted.

### Results

| Cypher | JavaScript |
|---|---|
| `NULL`, boolean, string | `null`, `boolean`, `string` |
| Integer | `number` when exact, `bigint` beyond 2^53 - 1. With `integers: 'bigint'`, always `bigint`. |
| Float | `number`. `NaN`, `Infinity`, `-Infinity` and `-0` are kept. |
| Date, datetime, duration, point | `LocalDate`, `LocalDateTime`, `Duration`, `Point` |
| List, map | `Array`, plain object |
| Node | `{ id, labels, properties }` |
| Relationship | `{ id, type, startId, endId, properties }` |
| Path | `{ nodes, relationships }` |

The value classes expose their fields and `toString()` (ISO-8601, or WKT for `Point`). `LocalDateTime.toDate()` returns a JS `Date` that reads the wall clock as UTC and drops sub-millisecond digits.

`JSON.stringify` throws on a `bigint`. This is a JavaScript rule. Pass a replacer or define `BigInt.prototype.toJSON` when a result may hold one.

## Streaming rows

`graph.stream(cypher, params?, options?)` runs a read-only query and returns an async iterator of row objects. Rows are the same objects `executeRead` puts in `rows`.

```js
for await (const row of graph.stream('MATCH (p:Person) RETURN p.name AS name')) {
  console.log(row.name);
}
```

- **Options:** `timeoutMs`, `rowLimit`, `maxWorkUnits` as for `executeRead`, plus `batchSize` (default 1000, minimum 1).
- **Event loop:** the stream converts rows to objects one at a time and returns to the event loop after every `batchSize` rows. Timers and I/O run between batches, so a large result no longer stalls the loop for one long conversion.
- **Memory:** the engine builds the whole result before the first row arrives. The result stays in memory until the stream ends, you `break`, or the stream is garbage collected.
- **Early exit:** `break`, `return()` and a thrown error in the loop body release the result. Writes, `close()` and new streams work straight away.
- **Errors:** a bad argument or a failing query rejects the first `next()` with the same typed error `executeRead` would reject with. The stream is finished after that.
- **Scope:** `stream()` runs on the graph, not inside a transaction. A mutating statement rejects `InvalidArgument`.

## Cancelling a call

`executeRead`, `executeWrite`, `tx.run` and `stream()` accept `signal`, a standard `AbortSignal`, in their options.

```js
const ac = new AbortController();
setTimeout(() => ac.abort(), 2000);
await graph.executeRead(slowQuery, null, { signal: ac.signal }); // rejects Cancelled
```

- **Already aborted:** the call rejects `Cancelled` without queuing.
- **Queued:** abort rejects at once and the call never runs.
- **Running:** abort stops the query on its worker. The promise rejects `Cancelled`, and `err.cause` is `signal.reason`.
- **Writes:** a cancelled `executeWrite` publishes nothing. A cancelled write in a transaction fails the statement and aborts the transaction; a cancelled read leaves it open.
- **Streams:** abort stops further batches and rejects the pending `next()`; later `next()` calls resolve `done`.
- **Listeners:** the abort listener is removed when the call settles, a stream ends, or `return()` runs.
- **Scope:** `transaction(fn)` takes no signal; pass one to the `tx.run` calls inside the callback.

## Transactions

`graph.begin(options?)` starts a transaction on a snapshot of the graph and resolves to a `Transaction`. Settle every transaction with `commit()` or `rollback()`.

```js
const tx = await graph.begin();
try {
  await tx.run('CREATE (:Item {sku: $sku})', { sku: 'a-1' });
  await tx.commit();
} catch (e) {
  await tx.rollback();
  throw e;
}
```

`graph.transaction(callback, options?)` does the settling for you. It commits when the callback's promise resolves and rolls back when it throws.

```js
await graph.transaction(async (tx) => {
  await tx.run('MATCH (a:Account {id: $id}) SET a.balance = a.balance - $n', { id: 1, n: 10 });
}, { retries: 3 });
```

| Behavior | Detail |
|---|---|
| Conflict | `commit()` rejects with `TransactionConflict` when another writer committed first. The code is retriable. |
| `retries` | A lost race re-runs the callback on a fresh transaction, up to `retries` times. Keep the callback free of side effects outside the graph. |
| `readOnly: true` | The transaction rejects writes. |
| Dropped transaction | A transaction that is garbage-collected unsettled rolls back. |
| Closed graph | `graph.close()` rolls back open transactions. Their next call rejects with `TransactionClosed`. |
| Finished transaction | `tx.finished` is true after commit, rollback or abandonment. `rollback()` is then a no-op. |

## Errors

Every rejection is an `Error` with `name === 'KgliteError'` and a string `code`. There is no class hierarchy: branch on `code`, not `instanceof`.

```js
try {
  await graph.executeWrite(cypher);
} catch (e) {
  if (e.name === 'KgliteError' && e.code === 'WriterLeaseHeld') {
    console.error(`held by pid ${e.holder.pid}`);
  }
}
```

| Code | Raised when |
|---|---|
| Engine codes such as `CypherSyntax`, `CypherTimeout`, `ConstraintViolation`, `OntologyViolation` | The engine rejects the statement. They match the other bindings. |
| `InvalidArgument` | An argument or parameter is invalid, or `executeRead` got a write. |
| `WriterLeaseHeld` | Another writer owns the path. `e.holder` carries `pid`, `since`, `label` and `self`. |
| `QueueFull` | The worker queue is at capacity. Retry later. |
| `Closed` | The graph handle is closed. |
| `ReadOnly` | A write on a `readOnly` graph. |
| `NotDurable` | `sync()` on a graph without a write-ahead log. |
| `TransactionClosed` | The transaction is already committed, rolled back or aborted. |
| `TransactionConflict` | A commit lost an optimistic race. |
| `Internal` | A panic inside the addon, contained as a rejection. |

`holder.self` is true when the holder is the current process, which means an earlier handle was opened and never closed. `pid`, `since` and `label` are absent when the holder had not published them.

## Event loop and threads

Queries run on a worker pool, never on the JavaScript thread. The pool has `min(4, cores)` threads. Set `KGLITE_NODE_THREADS` to change it.

Converting a result to JavaScript objects runs on the JavaScript thread at about 0.5 µs per row. A query that returns millions of rows stalls the event loop for that conversion. Bound results with `LIMIT` or `rowLimit`, or read them with [`stream()`](#streaming-rows).

## Several processes

| Rule | Detail |
|---|---|
| One writer | A writable `open()` takes a writer lease on the path. A second writer rejects with `WriterLeaseHeld`, or waits up to `lockTimeoutMs`. |
| Readers | `readOnly: true` takes no lease and loads the last checkpoint. |
| Reader freshness | A reader sees only the last checkpoint. Writes still in the write-ahead log are invisible to it until the writer checkpoints. |

Call `graph.checkpoint()` on the writer when readers need current data.

## One file format

The `.kgl` file is the same one the Python wheel and the Bolt server read and write. A graph built in Python opens in Node, and the reverse.

## Backup

`graph.backup(dest)` writes a consistent single-file `.kgl` copy while writers keep committing. It resolves to a report:

| Field | Meaning |
|---|---|
| `path` | The destination file. |
| `bytes` | Size of the published file. |
| `nodes`, `relationships` | Counts in the copy. |
| `graphVersion` | The graph's in-memory commit count at the snapshot. |
| `lsn` | Newest write-ahead-log position in the file. `null` without a log. A `bigint` beyond 2^53 - 1, or always with `integers: 'bigint'`. |
| `lockHoldMs` | How long writers were held off to fix the point in time. |
| `elapsedMs` | The whole call. |
| `preparedCopy` | True when the snapshot needed a private copy of the graph first. |

- The copy is a prefix of the committed history, with no gaps.
- The destination holds either the previous file or the complete new one. A crash never leaves a partial file.
- A `readOnly` graph can be backed up.
- A backup onto the live graph's own path, or an alias of it, is refused and changes nothing.
- A disk-mode graph is a directory. `backup()` rejects it; use another route to copy it.

## Ontology

An ontology makes the engine judge every write against declared rules. The Python guide documents the rule model: see [Write-time enforcement](../python/guides/ontology.md).

| Call | Behavior |
|---|---|
| `graph.declareOntology(ontology)` | Declares or replaces the ontology. It takes an object or a JSON string and resolves to `{ warnings }`, the `warn`-level findings over stored data. The declaration persists across reopen. |
| `graph.clearOntology()` | Removes the ontology and lifts enforcement. |

Both reject with `ReadOnly` on a `readOnly` graph. A malformed declaration rejects with `InvalidArgument`.

A refused write rejects with `OntologyViolation`. The error carries:

| Field | Meaning |
|---|---|
| `rule` | The rule that fired: `required_property`, `property_type`, `closed_labels`, `domain` or `range`. |
| `entity` | `'node'` or `'relationship'`. |
| `entityType` | The label or relationship type. |
| `property` | The offending property, or `null` for a rule without one. |
| `report` | Per-rule breakdown of a refused declaration, as `{ rule, entity, entityType, property, count }` entries. Empty for a refused write. |

An `error`-level violation rolls the write back. In a transaction it rolls back the earlier statements too. A declaration over data that already breaks an `error` rule is refused, and the previous ontology stays.

## Embedders

`graph.setEmbedder(name, embed, options?)` registers a JavaScript function that turns text into vectors. Cypher then embeds text through it: `text_score(n, prop, 'query text')`, `db.embeddings.embed` and `db.embeddings.query({text})`.

```js
graph.setEmbedder('minilm', async (texts) => model.embed(texts), { dimension: 384 });
await graph.executeWrite(`MATCH (d:Doc) WITH collect(d) AS docs
  CALL db.embeddings.embed({type: 'Doc', text_column: 'text', nodes: docs}) YIELD embedded RETURN embedded`);
const top = await graph.executeRead(
  "MATCH (d:Doc) RETURN d.id AS id, text_score(d, 'text', $q) AS s ORDER BY s DESC LIMIT 5",
  { q: 'how do I reset my password' },
);
```

| Part | Behavior |
|---|---|
| `embed(texts)` | Takes `string[]`. Returns, or resolves to, one array (or typed array) of finite numbers per text. |
| `name` | Registration label. It is the model identity stamped on stored embeddings unless `modelId` is set. |
| `dimension` | Vector width. Omitted: learned from the first vector, with one probe call when a statement needs the width first. A vector of another width is an error. |
| `modelId` | Model identity for provenance. Default is `name`. |
| `timeoutMs` | How long one call may wait for the function. Default `120000`. |

`graph.clearEmbedder(name?)` removes it and returns whether one was removed. With `name`, it removes only that registration. One embedder is active per graph; a second `setEmbedder` replaces the first. The embedder is not saved: register it again after every `open`.

A function that throws, rejects, returns the wrong shape or the wrong width fails the query with a `KgliteError` (the statement's code, normally `CypherExecution`) naming the cause. The process stays up.

- **Threads.** The engine calls the function from a pool thread, and the function runs on the JavaScript thread. The event loop stays free while an async embedder works.
- **Deadlock limit.** A pool thread waits for each call. An embedder that itself awaits queries on the same graph, while every pool thread is waiting on embedders, cannot finish. The call ends after `timeoutMs` with an error. Keep the function free of graph calls.
- **No synchronous path.** Embedding never runs on the JavaScript thread. Code that would reach it there fails with an error instead of hanging.

## Not in version 1

- Streaming rows out of the engine. `graph.stream()` streams the conversion to JavaScript objects only; the engine still builds the whole result first.
