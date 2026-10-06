# Bolt server

`kglite-bolt-server` exposes the embedded KGLite engine over Bolt v5.x. The
official Neo4j Python, JavaScript, and Java drivers are regression-tested.
Other Bolt v5 clients are untested. They must stay within the documented
protocol and [Cypher dialect](../reference/cypher-reference.md) limits.

## What this server is (and is not)

One process owns one graph and serves Bolt clients over the wire. Use it for
trusted or loopback access, or behind a proxy that owns authentication and
authorization. Reads run against snapshots and scale across concurrent
sessions.

- **No user directory and no RBAC.** `--auth basic` configures a single shared
  credential. The authenticated principal is validated at LOGON and not
  stored, so there is no per-session identity to authorize against. `--auth
  none` accepts any LOGON.
- **No high availability and no replication.** There is no failover, cluster,
  or bookmark/causal-consistency protocol.
- **One writer.** Writes serialize at commit within the process. One writable
  server per graph is enforced by a cross-process lease (see *Operations and
  security* below).
- **The graph file is not rewritten continuously.** Every commit is appended
  to a write-ahead log as it is acknowledged. The `.kgl` itself changes only
  when a checkpoint runs: `CALL db.checkpoint()`, `--checkpoint-interval`, or
  `--save-on-exit`. Turn the log off with `--durability off` and a commit is
  process-local until one of those runs. The file is then whatever it was when
  the server opened it. See *Durability* below for what each level costs and
  what it leaves behind.

If you need per-user access control, use a different shape. The supported
pattern is the
[derived-index / traversal-component pattern](../python/guides/derived-index.md#an-embedded-traversal-component-behind-your-api).
The engine is embedded behind your own API, and that API owns authentication,
authorization, and write policy.

For the feature-by-feature carry-over table (routing URIs, auth, auto-commit
mutations, OCC, and multi-database), see
[Migrating from Neo4j to KGLite](../python/migrations/neo4j-to-kglite.md).

## Install and start

```bash
cargo install kglite-bolt-server
kglite-bolt-server --graph /data/app.kgl
```

An existing `.kgl` opens in the storage mode it was saved in. A disk-graph
directory opens disk-backed. A missing path is an error unless creation is
explicit:

```bash
kglite-bolt-server --graph /data/new.kgl --storage memory
# --storage mapped|disk selects the other creation modes
```

`--storage` on an *existing* graph is a conversion request, not a no-op. A
memory-saved graph served with `--storage mapped` is converted to mapped before
the listener binds, and the startup log records `converted_from`.

A disk graph is a directory rather than a file, so converting into or out of
disk mode has no in-place form. Those requests fail startup naming
`enable_disk_mode()` instead of serving a mode nobody asked for. Omit the flag
to serve whatever the graph recorded.

Important options (run `--help` on the installed version for the authority):

| Option | Purpose |
|---|---|
| `--bind`, `--port` | listener, default `127.0.0.1:7687` |
| `--storage memory\|mapped\|disk` | create a missing graph in this mode, or convert an existing one to it (memory ⇄ mapped; disk directions refused) |
| `--readonly` | reject mutations at execution |
| `--durability full\|normal\|off` | what an acknowledged commit survives, default `normal` (see *Durability*) |
| `--save-on-exit` | checkpoint the served graph back to `--graph` on `SIGINT`/`SIGTERM` |
| `--checkpoint-interval SECS` | checkpoint the served graph on a timer |
| `--checkpoint-wal-mib MIB` | checkpoint when the log passes this size, default `16` while a log is kept, `0` disables |
| `--auth none\|basic`, `--auth-user`, `--auth-pass` | Bolt LOGON policy |
| `--idle-timeout`, `--max-sessions`, `--max-message-size` | resource bounds |
| `--advertise-addr HOST:PORT` | address returned to `neo4j://` routing clients |
| `--tls-cert`, `--tls-key` | PEM TLS pair for `bolt+s://` / `neo4j+s://` |

## Driver example

```python
from neo4j import GraphDatabase

driver = GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None)
with driver.session() as session:
    rows = session.run("MATCH (n) RETURN count(n) AS n").data()
```

With basic auth, pass the configured `(user, password)`. Use `neo4j://` only
when you want routing behavior. Set `--advertise-addr` to an address reachable
by the client, especially behind a proxy or when binding `0.0.0.0`.

## Valid time

On a graph that declares validity intervals, a statement with no
`FOR VALID_TIME` prefix reads as of today (UTC). `FOR VALID_TIME ALL` reads
every version.

- `--valid-time-default {today|all|YYYY-MM-DD}` sets the instant an unprefixed
  statement reads.
- A statement's own prefix wins.
- The setting is never written into the `.kgl` file. Without the flag, a
  default the graph stored in its file applies, else today.
- Each result's `kglite.temporal` summary key reports the `source` (`default`,
  `explicit`, `all` or `skipped:<reason>`), the instant and the rows the
  context hid.

## Transactions and errors

The backend uses native KGLite sessions and transactions, not Python or the
GIL. Reads and schema statements may auto-commit. **All data writes must be
explicit driver transactions.** An auto-commit data mutation is rejected rather
than run. That covers `CREATE`/`INSERT`, `SET`/`REMOVE`, a delete form, and
`MERGE`. Use the driver's `execute_write` equivalent; a plain `session.run`
remains auto-commit.

### Schema statements

`CREATE INDEX`, `DROP INDEX`, `CREATE CONSTRAINT` and `DROP CONSTRAINT` run
through a plain `session.run`, as they do on Neo4j, and publish as a
transaction of their own. The result summary reports query type `s`. A refused
form, such as a `FULLTEXT` index, publishes nothing.

| Where | Neo4j | KGLite |
|---|---|---|
| `session.run` (auto-commit) | runs | runs |
| `execute_write` / `begin_transaction`, schema only | runs | runs, commits with the transaction |
| Same transaction writes data and schema | refused | **runs**, commits atomically |
| `CALL db.checkpoint()` | auto-commit only | auto-commit only, refused inside a transaction |

The mixed case is the one deliberate difference. A script that works on Neo4j
works here unchanged, and a transaction that groups an index with the data it
serves commits or rolls back as one.

Concurrent writers serialize at commit. A transaction committing against a
stale snapshot conflicts with a retriable status code. Driver-managed
transactions (`execute_write` and its per-language equivalents) retry the unit
of work by themselves. Hand-rolled `begin_transaction` code needs its own retry
loop.

Error codes:

- The auto-commit refusal is published as `Neo.ClientError.Request.Invalid`.
  That is a client error with a client-side remedy.
- A `--readonly` server or a disk-mode graph answers with
  `Neo.ClientError.Security.Forbidden` instead. No rewrite of the request
  helps there.
- KGLite typed errors map to Neo4j status codes for syntax, schema, timeout,
  access-mode, conflict, and execution failures.

### Transaction timeouts

KGLite does not yet implement Bolt transaction timeouts. This server applies
**no query deadline of its own**. That is a declared divergence from the Python
API and the MCP server, which both apply the shared 180,000 ms default.
"Absent `tx_timeout` means no timeout" is the Neo4j wire contract, and a driver
that wants a bound sends one.

- A top-level `tx_timeout` of zero, NULL, or absent means no timeout.
- Any nonzero value is rejected before RUN or BEGIN changes state
  (`Neo.ClientError.Request.Invalid`).
- A `tx_timeout` key nested inside `tx_metadata` remains ordinary user
  metadata.

## Timezone-aware datetime parameters

A zoned PackStream temporal (`DateTime`, `DateTimeZoneId`, `Time`) is
**refused** with `Neo.ClientError.Request.Invalid`. KGLite's temporal values
are zoneless, so there is no lossless translation, and silently dropping the
zone would corrupt a driver's round-trip. Send `LocalDateTime`, `LocalTime` or
`Date` instead.

The Python API deliberately differs. It *converts* an aware `datetime` to
naive UTC. Its own Cypher `datetime()` constructor already does that to an
offset-bearing literal, and there is no wire type on that side to corrupt. See
{doc}`../python/value-projection` for the Python half.

The standing Bolt correctness and differential suites lock the supported
behavior. Avoid relying on an exact test/query count or a particular driver
patch version. CI exercises the complete current corpus.

### Write concurrency

Reads run against snapshots and do not block each other or writers. Writes are
the serialized resource. Every write is an explicit transaction, transactions
work independently, and they order at commit.

A transaction whose snapshot was overtaken loses the race and conflicts with
the retriable status code. A driver-managed transaction re-runs the unit of
work on a fresh snapshot without your code seeing the conflict at all.

More writer clients do not create additional commit capacity. Contention shows
up in retries and end-to-end latency. To reduce it:

- Batch related writes into one transaction, for example with `UNWIND $rows`,
  to amortize per-transaction work.
- Tune the driver's transaction retry budget if tail latency matters.

The opt-in `tests/benchmarks/test_bench_bolt_writers.py` measures writer count,
batch size, and durability on the current code. Correctness is pinned by
`tests/test_bolt_server_transactions.py` and
`tests/test_bolt_server_concurrency.py`.

## Durability

Two independent mechanisms decide what a stopped or killed server leaves
behind:

- A **write-ahead log** records each commit to a sidecar file as it is
  acknowledged. An interruption costs at most what the log does not hold.
- A **checkpoint** rewrites the whole `.kgl` from the committed graph and
  truncates the log.

They are complements, not alternatives. The log bounds the window while the
server runs. The checkpoint folds the log back into the file.

### Levels (`--durability`)

`--durability full|normal|off` selects what an *acknowledged* commit survives.
You can also set `KGLITE_BOLT_DURABILITY=<level>`; the flag wins if both are
set.

The frame is written **before** the client is told the commit succeeded. A
commit whose frame cannot be written is therefore not applied at all, and is
reported as a failure. The server never acknowledges a write it then discards.

| Level | An acknowledged commit survives | It does not survive |
|---|---|---|
| `full` | the server process dying, and — by asking the device for a write barrier before acknowledging — an OS crash or power loss | media failure, or anything the filesystem itself loses |
| `normal` (default) | the server process dying: `SIGKILL`, an OOM kill, a panic. The frame is in the kernel's page cache | an OS crash or power loss before the kernel writes that page out |
| `off` | nothing by itself — commits stay in this process until a checkpoint rewrites the file | the process ending at all, unless a checkpoint ran first |

What is pinned by test is the process-kill case. At `full` and at `normal`,
committing over Bolt and then `SIGKILL`ing the server with no checkpoint of any
kind leaves the `.kgl` byte-identical. The restarted server replays the commit
out of the log. `off` is the control: the same write is gone. Nothing in a
user-space test can take the page cache or the power away. The
`full`-versus-`normal` distinction above is therefore a statement about the
barrier each level takes, not a measured one.

The default is `normal`. `full` takes a device barrier for every commit while
holding the commit lock, so it can sharply reduce write throughput and increase
contention. Enable it when acknowledged commits must survive an OS crash or
power loss, and measure the effect on representative storage and workload. The
corresponding sweep is
`tests/benchmarks/test_bench_bolt_writers.py::test_durability_sweep`
(`-m "benchmark and bolt_stress"`).

### Recovery on startup

Recovery is unconditional and runs before the listener binds, at every level.
Opening a path is a decision about that path's *data*, not only about how
future writes will be logged.

- At `full` and `normal`, a sidecar holding commits the `.kgl` does not
  contain is replayed into the graph.
- At `off`, the same sidecar is a startup error naming both ways out. Restart
  at `full` or `normal` to replay the commits, or move the sidecar aside to
  discard them deliberately. A server that would otherwise serve a graph
  missing acknowledged writes does not start.
- Frames the checkpoint already contains are not grounds to refuse, and open at
  every level. They are the harmless residue of a crash between a checkpoint's
  file write and its log truncation.

### Server-facts verbs

Besides `db.checkpoint()` (below), three read-only verbs are answered at the
Bolt layer. They report server state the engine does not hold. They are the
introspection calls Neo4j clients make on connect:

- **`CALL dbms.components()`** — name / versions / edition, following
  `--neo4j-compat` (see *Driver identity*). Edition is always `community`.
- **`CALL dbms.showCurrentUser()`** — the configured `--auth-user`
  (or `neo4j` under `--auth none`); roles and flags are empty. Answered from
  server config: there is deliberately no per-session principal.
- **`SHOW DATABASES`** — one row named `neo4j` (matching the routing
  default), `default`/`home` true, `access`/`writer` reflecting
  `--readonly`. The session `database=` field remains accepted-and-ignored;
  this row is informational.

All three take an optional `YIELD` naming a subset of their declared columns,
like `db.checkpoint()`. They exist only over Bolt, because in-process bindings
have no server to describe. To see what any client sends on connect, run the
server at `RUST_LOG=debug`. Every incoming query is logged.

### Checkpoints

Four routes rewrite the served `.kgl`, and all four are the same operation:
flush the log, stamp the checkpoint position, write the file, truncate the log.

- **`CALL db.checkpoint()`** — on demand, over the wire. It is a *bolt-server
  verb*, not an engine procedure. It exists only over Bolt, and embedded
  bindings keep their own save calls. It answers in Neo4j's `success, message`
  shape, and an optional `YIELD` of either or both columns is honoured. A
  checkpoint whose graph has not changed since the last one *in this process*
  is skipped and says so. The first call of a process always writes, because
  the file may predate the process.
- **`--checkpoint-interval SECS`** (`KGLITE_BOLT_CHECKPOINT_INTERVAL`) — on a
  timer. The interval task and the verb share one recorded version, so a
  checkpoint by either makes the next tick a skip. An idle server does not
  rewrite its file. A failed tick is logged as an error and the server keeps
  serving. The interval is validated at startup rather than starting a server
  that silently never checkpoints.
- **`--checkpoint-wal-mib MIB`** (`KGLITE_BOLT_CHECKPOINT_WAL_MIB`) — on log
  size, and **on by default** at `full` and `normal`. Every 10 seconds the
  server compares the sidecar with the threshold (default 16 MiB) and with the
  `.kgl`. It checkpoints when the log is at least as large as both, because a
  log bigger than the file it extends costs more to replay than to rewrite.
  `0` disables it. An explicit value is refused with `--readonly` and for
  disk-mode graphs. The default does not apply there, or at `off`, where
  there is no log. It shares the recorded version with the verb and the
  interval task, so an unchanged graph is skipped.
- **`--save-on-exit`** (`KGLITE_BOLT_SAVE_ON_EXIT`) — once, on `SIGINT` or
  `SIGTERM`, after periodic checkpointing has been stopped. The saved graph
  version is logged. A failed exit save is logged as an error *and* exits
  non-zero, so a supervisor sees it. Connections are not drained, so a commit
  racing shutdown can land after the save. The logged version is how you tell
  that apart from a save that never ran. Under a log, the commit is still in
  the sidecar and the next start replays it.

A checkpoint pauses writers and new snapshots for its duration, because
`Session::save` holds the session lock for the full save. Readers already
holding a snapshot are unaffected. Time one `CALL db.checkpoint()` on a
representative graph before choosing the interval.

Retention is one: each checkpoint atomically replaces the previous file. Use
filesystem tooling, such as a snapshot, a copy, or a backup job, if you want
history.

### The sidecar file

At `full` and `normal` the server keeps `<graph>-wal` beside the graph file.
Every checkpoint truncates it back to its header. Between checkpoints it grows
by under a hundred bytes per single-node commit. Multiply that by your commit
rate to size it.

The log is bounded by default: `--checkpoint-wal-mib` checkpoints once it
passes 16 MiB (and the size of the `.kgl`). Set `--checkpoint-wal-mib 0` and
nothing else, and the sidecar grows with uptime and a restart replays all of
it. Replay folds the log frame by frame, so its memory follows the distinct
nodes the log names, not the log's length. On macOS a 6 MB and a 56 MB log over
a bounded set of nodes each restarted 7-9 MiB above an idle server, while a log
that keeps creating and deleting new nodes costs up to five times its size. `--checkpoint-interval` adds a timer on top.
Neither makes commits safer, because the log already did that. They keep
replay time and sidecar size bounded.

Back up the sidecar with the graph, or checkpoint before copying the `.kgl`
alone. A `.kgl` copied while a sidecar runs ahead of it is missing the commits
the sidecar holds. The engine refuses the dangerous half of this by itself. A
non-durable open, and a save, over a path whose sidecar runs ahead are errors
rather than silent data loss.

### Refusal matrix

Two configurations cannot carry a log or a checkpoint:

- `--readonly`: a server that never commits has nothing to log and nothing to
  write back.
- Disk-mode graphs: a disk graph commits by publishing an immutable generation,
  so it keeps no logical log. Every disk save publishes a *new* generation that
  nothing prunes, so repeated checkpoints would grow the directory without
  bound.

An explicitly requested level or feature is refused there. The *default* level
degrades instead, so flipping the default did not turn every read-only and
disk-mode server into a startup error:

| Configuration | `--durability full`/`normal` (asked for) | `--durability` (default) | `--save-on-exit`, `--checkpoint-interval`, `--checkpoint-wal-mib` (asked for) | `CALL db.checkpoint()` |
|---|---|---|---|---|
| `.kgl`, writable | serves at that level | serves at `normal` | supported | supported |
| `--readonly` | startup error | serves at `off`, logged | startup error | `Neo.ClientError.Security.Forbidden` |
| disk-mode graph | startup error | serves at `off`, logged | startup error | `Neo.ClientError.Security.Forbidden` |

The one refusal that is about *data* rather than configuration is `off` over a
sidecar that runs ahead of the file, above. A level nobody asked for replays it
instead, which is what makes the default safe to inherit.

Environment mirrors are refused exactly as the flags are. A mistyped level or
interval is a startup error rather than a server that silently logs nothing.

`CALL db.checkpoint()` is also refused inside an explicit transaction. It
writes the *committed* graph, which by definition excludes that transaction's
uncommitted writes. Commit first and call it in auto-commit.

`tests/test_bolt_server_durability.py` (`-m bolt`) pins the behavior above. It
includes the `SIGKILL`-and-restart tests behind each level, the
checkpoint-truncates-the-log test, and every row of this matrix.

## Driver identity (`--neo4j-compat`)

By default the handshake `server` agent and the `CALL dbms.components()` row
(name, versions, edition—always `community`) report
`kglite-bolt-server/<version>`. Under `--neo4j-compat` they report the
Neo4j-compatible spelling. The separate `bolt_agent` metadata always names
`kglite-bolt-server/<version>` honestly. Compatibility mode changes only the
fields clients use for their Neo4j product gate.

Two client families need the compatible spelling:

- the official **Java** driver requires a `Neo4j/` agent prefix and refuses
  the connection outright without one:

  ```
  UntrustedServerException: Server does not identify as a genuine Neo4j
  instance: 'kglite-bolt-server/<version>'
  ```

- **GUI clients** (Neo4j Browser, G.V(), and other IDEs) read
  `dbms.components()` to decide product and feature support, so they need
  the flag too. Under it the row reports `Neo4j Kernel` / `5.26.0` /
  `community`. The official Python and JavaScript drivers accept the honest
  default.

Enable compatibility mode to serve those clients. Either route works, and the
flag wins if both are set:

```bash
kglite-bolt-server --graph graph.kgl --neo4j-compat
KGLITE_BOLT_NEO4J_COMPAT=1 kglite-bolt-server --graph graph.kgl
```

The agent then becomes `Neo4j/5.26.0 (kglite-bolt-server/<version>)`. That is
enough of a Neo4j spelling to pass the driver's check. The real product is
retained, so the server stays identifiable in logs, in driver errors, and
through `ServerInfo.agent()`. The handshake `server` field and
`dbms.components()` row change; `bolt_agent` keeps reporting kglite.

The variable accepts `1`, `true`, `yes` or `on` (any case). That is the useful
form for container images and unit files, where adding an argument means
rebuilding or editing a unit.

The mode is off by default on purpose. Presenting as a different product is the
operator's call, and the identity is never switched automatically. When a
driver that enforces the check connects with compatibility off, the server logs
a warning naming both activation routes. An operator can then diagnose it from
the server log instead of a client stack trace.

## Operations and security

- **Loopback is the safe default.** If you expose the server remotely, enable
  basic auth and TLS, or terminate TLS/auth at a trusted proxy/firewall
  boundary.
- **Bound resources on untrusted networks.** Set `--max-message-size`,
  `--max-sessions`, and an idle timeout whenever the listener is reachable from
  an untrusted network. They bound resource use per connection; they are not an
  access-control boundary.
- **Use `--readonly` for read-only analytical instances** sharing the same
  graph file, and for agent connections that do not need writes. A `--readonly`
  server is a second process opening the same graph, not a replica. It serves
  what the file contained when it opened it. Beside a durable writer, that
  excludes whatever the writer has committed to its sidecar since the last
  checkpoint (see *Durability*). A read-only server keeps no log itself and
  serves at `--durability off`.
- **One writable server per graph.** A server started without `--readonly`
  takes the same cross-process writer lease as `kglite.open()` *before* it
  reads the graph, and holds it until shutdown.
  - A second writable server, a CLI write, or `kglite.open()` on that path
    fails at startup naming the holding process, instead of racing it to
    overwrite at save time.
  - The refusal is immediate rather than a wait, so a supervisor's restart
    policy governs the retry.
  - A write-enabled MCP server on that path boots fine and is refused at its
    first mutation instead, because it takes the lease lazily. A library
    `load()` + `save()` still opts out of leases (pending WAL recovery can
    separately refuse it). See
    [the MCP server's operating notes](mcp-server.md#the-writer-lease-and-several-servers-on-one-file).
  - `--readonly` servers take no lease and start alongside a live writer.
  - Because the lease is exclusive, the graph's write-ahead sidecar has exactly
    one writer too.
- **Back up the complete graph before upgrades.** Include the `<graph>-wal`
  sidecar, or back up after a `CALL db.checkpoint()` that folds it in. See
  [Import and Export](../python/guides/import-export.md) and *Durability*.
- **Use release benchmarks/CI reports for performance claims.** This operator
  page intentionally avoids unversioned hardware-specific numbers.
