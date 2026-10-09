# Error handling

KGLite exposes a typed Python exception hierarchy for engine, Cypher, schema,
transaction, and storage failures. Catch the narrowest class you can recover
from; catch `kglite.KgError` when every KGLite engine failure has the same
handling policy.

## Exception hierarchy

```text
Exception
└── kglite.KgError
    ├── kglite.CypherError
    │   ├── kglite.CypherSyntaxError
    │   ├── kglite.CypherTimeoutError
    │   ├── kglite.CypherExecutionError
    │   └── kglite.CypherTypeMismatchError
    ├── kglite.SchemaError
    ├── kglite.ValidationError
    ├── kglite.ExprError
    ├── kglite.ConstraintError
    │   ├── kglite.ConstraintViolationError
    │   │   └── kglite.OntologyViolationError
    │   └── kglite.ConstraintCreationError
    ├── kglite.TransactionConflictError
    ├── kglite.NodeNotFoundError
    ├── kglite.ConnectionNotFoundError
    ├── kglite.PropertyNotFoundError
    ├── kglite.FileError
    ├── kglite.FileFormatError
    ├── kglite.FileIoError
    │   └── kglite.WriterLeaseHeldError
    ├── kglite.LoadMemoryLimitError
    ├── kglite.ArgumentError
    │   └── kglite.ReadOnlyError
    ├── kglite.MissingArgumentError
    ├── kglite.InternerCollisionError
    └── kglite.InternalError
```

Position and timeout details:

- `CypherSyntaxError` always has `.line` and `.col` attributes (either may be
  `None`).
- `CypherExecutionError` has them when the executor can identify the source
  position.
- Timeout messages report the configured limit and the elapsed time. Both are
  measured from when the call resolved its deadline, so time spent converting
  parameters or forking a transaction's working copy counts toward it.

## Stable codes

Every instance carries `.code`, a stable classifier string. Branch on that rather
than on message prose, which is free to improve between releases:

```python
try:
    graph.cypher(query)
except kglite.KgError as exc:
    log.warning("kglite failed", extra={"kglite_code": exc.code})
```

`.code` is also readable on the concrete classes themselves
(`kglite.ConstraintViolationError.code == "ConstraintViolation"`), so a dispatch
table can be built up front.

- `.code` is `None` on the three abstract bases (`KgError`, `CypherError`,
  `ConstraintError`), which each span several codes.
- The same strings appear as `KGLITE_STATUS_*` in the C ABI and drive the Bolt
  `Neo.*` status mapping. One code therefore means the same thing in every
  binding.

## Constraint violations

A write that breaks a declared UNIQUE / NOT NULL / NODE KEY / `IS :: TYPE`
constraint raises `ConstraintViolationError`, from **every** write path:
`cypher()` and the bulk loaders alike.

- Relationship constraints (`FOR ()-[r:T]-() REQUIRE r.p IS NOT NULL` /
  `IS :: TYPE`) raise the same exception, with a message written in relationship
  words.
- Declaring a constraint the stored data already violates is a different problem
  with a different fix. It raises the sibling `ConstraintCreationError`.
- Both subclass `ConstraintError`.
- `OntologyViolationError` (`.code == "OntologyViolation"`) is raised when the
  declared ontology refuses a write, or refuses a declaration the stored data
  already violates. It subclasses `ConstraintViolationError`, so an existing
  `except kglite.ConstraintViolationError` clause still catches it.
- Bolt reports it as `Neo.ClientError.Schema.ConstraintValidationFailed`; the C
  ABI status is `KGLITE_STATUS_CODE_ONTOLOGY_VIOLATION` (22).

```python
try:
    graph.cypher("CREATE (u:User {email: $email})", params={"email": email})
except kglite.ConstraintViolationError:
    raise Conflict("that email is already registered")
```

The message names the constraint, the property, and the offending value, so it is
worth logging. The type and `.code` are the contract.

## Transaction conflicts

`Transaction.commit()` raises `TransactionConflictError` when the graph moved
since `begin()`. Nothing was applied, so the fix is to re-run the work against a
fresh `begin()`. See {doc}`transactions` for `retry_on_conflict`, which is that
loop.

```python
try:
    tx.commit()
except kglite.TransactionConflictError:
    ...  # rebuild the transaction and try again
```

## A write on a read handle

Four handles refuse writes, and all four refuse with the same class and the same
code: `ReadOnlyError` / `ReadOnly`. `ReadOnlyError` subclasses `ArgumentError`, so an
existing `except kglite.ArgumentError` still catches it. The code is core's
`ReadOnly`, the identity the Node binding (`ReadOnly`), the C ABI (status 24) and
the Bolt server (`Neo.ClientError.General.ReadOnly`) report for the same refusal.

| Handle | Refuses |
|---|---|
| `Session.cypher()` | use `Session.execute()` for serialized writes |
| `FrozenGraph.cypher()` | an immutable snapshot; mutate the source graph and re-`freeze()` |
| A transaction from `begin_read()` | use `begin()` for read-write |
| A graph under `read_only(True)` | `read_only(False)` re-enables mutations |

One policy gets one class, so an application routes on the refusal without
matching four things:

```python
try:
    session.cypher(statement)
except kglite.ReadOnlyError as exc:
    assert exc.code == "ReadOnly"
```

The refusal is deliberately *not* `CypherExecutionError`. The query did not fail to
execute. It was aimed at a handle that does not take it, and a caller routes on
the class. It is a client error on the wire: `ReadOnly` maps to
`Neo.ClientError.General.ReadOnly` over Bolt (HTTP 403), while `CypherExecution`
and `InvalidArgument` map to `Neo.ClientError.Statement.ArgumentError` (HTTP 422
and 400).

A `CypherExecutionError` is a statement that failed on what it was given:

- a malformed function argument
- a property or declaration the query relies on that does not exist
- a stored value an operation cannot read

It is the query or the data to fix, not the server. Server faults are
`InternalError` / `FileIoError`.

### Unknown node types

The same rule covers an **unknown node type**. `properties()`,
`neighbors_schema()`, `sample()`, `describe(types=[...])`, `set_parent_type()` and
`set_temporal()` all raise `ArgumentError` for a type the graph does not have.

### Valid-time errors

**Valid time** follows the same split.

- **`CypherExecutionError`.** A statement under `FOR VALID_TIME AS OF` raises it
  when it does any of these:
  - writes
  - calls a procedure with no valid-time route
  - names an axis other than `VALID_TIME`
  - runs on a graph with no validity declaration

  `FOR VALID_TIME ALL` is exempt: it writes, and on a graph with no declaration it
  does nothing.
- **`CypherExecutionError`, no prefix.** On a graph that declares validity,
  `degree()`, `indegree()`, `outdegree()` and `shortest_path_length()` raise it
  too, with a hint to use `COUNT { (n)--() }` or `FOR VALID_TIME ALL`.
- **`CypherSyntaxError`.** A second context in one statement.
- **`ValueError`, from Python's `valid_at=`.** It raises before the query runs for
  an instant that is not a date or datetime, and for a query that already carries
  a context.
- **`ValueError`, from the fluent node filters** (`select()`, `valid_at()`,
  `valid_during()`). They raise it for bounds they cannot read or a field the type
  does not have.
- **`ArgumentError`.** `traverse(at=…)` on an undeclared relationship type, and an
  unreadable `date()` argument.

## Catching errors

```python
import kglite

try:
    result = graph.cypher(query, params=params, timeout_ms=30_000)
except kglite.CypherSyntaxError as exc:
    print(f"invalid query at {exc.line}:{exc.col}: {exc}")
except kglite.CypherTimeoutError:
    print("rewrite, scope, or explicitly increase the deadline")
except kglite.CypherError as exc:
    print(f"query failed: {exc}")
```

A timed-out Cypher query raises `CypherTimeoutError`. It does not return a partial
`ResultView`.

- Mutation execution restores a statement checkpoint on an execution error,
  timeout, or work-budget refusal, including direct `KnowledgeGraph.cypher()`
  calls.
- Previously successful statements remain intact.
- Use an explicit {doc}`Transaction <transactions>` when several statements must
  commit or roll back together.
- This is an execution guarantee, not rollback of later application-side result
  conversion or consumer errors.

For a broad engine boundary:

```python
try:
    graph = kglite.load("graph.kgl")
    rows = graph.cypher(query)
except kglite.KgError as exc:
    log.error("KGLite operation failed: %s", exc)
```

## Built-in Python exceptions

`KgError` is not a wrapper around every Python failure. Python-facing
protocols retain conventional exceptions:

| Situation | Exception |
|---|---|
| Missing result column or mapping key | `KeyError` |
| Invalid Python-side value or unsupported wrapper mode | `ValueError` |
| Wrong Python object or argument shape | `TypeError` |
| A query parameter outside the signed 64-bit integer range | `OverflowError` |
| Wrapper-side path opening | `FileNotFoundError` where documented |
| Borrow or object-lifecycle conflict | `RuntimeError` |
| User cancellation with Ctrl-C | `KeyboardInterrupt` |

`KeyboardInterrupt` is deliberately outside `KgError`, because an interrupt is a
user action, not a query fault. Catch it separately if the application needs
cleanup:

```python
try:
    graph.cypher(long_read, timeout_ms=0)
except KeyboardInterrupt:
    print("cancelled")
```

## Loading and recovery

Load failures are classifiable:

| Failure | Exception |
|---|---|
| A missing engine-managed path | `FileError` |
| Malformed, truncated, or unsupported saved data | `FileFormatError` |
| Other I/O failures | `FileIoError` |

### Load memory ceiling

A fourth case is not a failure of the file at all. `kglite.load(path,
max_load_mb=N)`, and the process-wide `KGLITE_MAX_LOAD_MB`, raise
`LoadMemoryLimitError` when the estimated peak is over the ceiling.

- Metadata-known terms refuse *before* decompression.
- An older portable file containing stored endpoint references needs a second
  conservative normalization-overlay check. It runs after the affected values are
  decoded, but before the private graph is changed or published.
- The graph is valid; this process cannot afford it. Rebuilding would not help,
  which is exactly why it is its own class.

To proceed, do one of these:

- Raise the ceiling.
- Pass `defer_index_rebuild=True` (usually the largest metadata term).
- Load it somewhere with more memory.

`kglite.estimate_load_memory(path)` reports the metadata-known terms as a dict. It
cannot see legacy reference values without decoding them. An affected file can
therefore pass that public estimate and still be refused on the additional
normalization term. The refusal message says which check fired.

```python
budget_mb = 512
try:
    graph = kglite.load("large.kgl", max_load_mb=budget_mb)
except kglite.LoadMemoryLimitError:
    # The index rebuild is the term usually worth dropping.
    graph = kglite.load("large.kgl", max_load_mb=budget_mb, defer_index_rebuild=True)
```

The ceiling compares an *estimate* read from the file's metadata head, not a
measurement, and it errs high on purpose. A ceiling set close to a graph's real
cost can therefore refuse a load that would have fitted. Set it where a failure is
what you want (a serving process that must not be killed by a file it did not
choose), not as a tight budget.

### Writer lease held

`kglite.open(path)` (and a `save()` onto a path another writer holds) raises
`WriterLeaseHeldError` when another process, or an earlier un-closed handle in
this one, holds the writer lease. It subclasses `FileIoError`, so an existing
`except kglite.FileIoError` still catches it, and its `.code` is
`"WriterLeaseHeld"`, the same code the C ABI (status 102), the Java wrapper, the
Node binding and the Bolt server's startup refusal report. It is retriable as it
stands, which is why it is not a plain I/O error.

`.holder` is a dict: `pid`, `since` (RFC-3339), `label` (each `None` when the
holder's record could not be read) and `self` (`True` when the holder is this
process).

```python
try:
    graph = kglite.open("app.kgl")
except kglite.WriterLeaseHeldError as exc:
    if exc.holder["self"]:
        ...  # an earlier open() in this process was never closed
    else:
        time.sleep(1)  # another process writes; retry, or kglite.load() to read
```

### Write-ahead-log failures

A commit the write-ahead log refuses (a full disk under `durable="full"` or
`"normal"`) raises `FileIoError` with `.code == "DurabilityFailed"`. `cypher()`,
the fluent writers, a transaction's `commit()` and `Session.run_write` all
answer with that one class and code. After the first refusal the handle is
latched: later logged writes, `save()` and `sync()` raise the same code until you
reopen the path.

### Save failures

The write half classifies the same way. A `save()`, `sync()` or `to_bytes()` that
fails on I/O raises `FileIoError` too. Examples are a full disk, a read-only
directory, and a failing device. `except kglite.KgError` therefore covers both
directions of the file lifecycle.

A `save()` *refused* before it touched the path is a `ValueError` instead. That
covers no remembered path, or a write-ahead sidecar running ahead of the target.
Nothing was written, and the fix is a different argument rather than a different
disk.

```python
try:
    graph = kglite.load("cache.kgl")
except kglite.FileError:
    graph = rebuild_from_source()
except kglite.FileFormatError:
    graph = rebuild_from_source()
```

A CSV export is an interoperability view, not a byte-for-byte graph backup.
Labels, schema, indexes, embeddings, time series, and some structured values are
not fully preserved. Keep the original source or a tested rebuild path. See
{doc}`guides/import-export` for the exact persistence and export contract.

## Concurrency conflicts

Direct `KnowledgeGraph` objects follow Python ownership and borrow rules. For
shared readers and writers, use `graph.session()`.

A transaction commits with optimistic concurrency control. A stale snapshot raises
a typed `KgError` instead of silently overwriting a newer commit. See
{doc}`transactions` and {doc}`/concepts/concurrency`.

## Other bindings

Rust code matches on `KgError` or the stable classifier `KgErrorCode`.

- The classifier also supplies canonical HTTP and Neo4j/Bolt status codes. Each
  binding still owns its response shape and lifecycle.
- The C ABI exposes the corresponding `KGLITE_STATUS_*` codes declared in the
  generated header.

See {doc}`/rust/c-abi` for ownership and status details.

`InternalError` represents a broken KGLite invariant. It is not a recoverable
user-input condition; report it with the complete message and a minimal
reproducer.

## See also

- {doc}`Python API reference <../autoapi/index>` — method-specific exceptions.
- {doc}`transactions` — rollback, optimistic commits, and shared sessions.
- {doc}`guides/cypher` — query deadlines, row caps, and diagnostics.
- {doc}`/rust/api-reference` — Rust error and execution-option boundary.
- {doc}`/rust/c-abi` — non-Rust binding status codes.
